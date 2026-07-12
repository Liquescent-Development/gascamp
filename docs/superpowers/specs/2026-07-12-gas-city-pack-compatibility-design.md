# Gas City pack compatibility — design

**Date:** 2026-07-12
**Issues:** [#80](https://github.com/Liquescent-Development/gascamp/issues/80) · [#84](https://github.com/Liquescent-Development/gascamp/issues/84) · [#85](https://github.com/Liquescent-Development/gascamp/issues/85)
**Incorporates:** `2026-07-12-camp-pack-imports-design.md` — the import machinery (source grammar, lock, verbs, `camp init` flow, error table) in full detail. That is the component spec for §7; this is the umbrella. Where they disagree, this wins.
**Status:** rev 3 — after a second adversarial review. Rev 2's fix for "the design refuses gastown" **introduced the biggest defect in the document**: it re-scoped v1 onto three packs chosen by reading only their *agents*, never their `pack.toml` or their formulas. Those packs need a transitive pack import (which rev 2 put out of scope) and `drain` (which rev 2 put in phase 5), and all 51 of their agents no-op under camp's worker environment. §3, §6.1–6.3 and §12 are the fix.

## 1. Why

README: camp is *"what k3s is to k8s"* for Gas City. **k3s runs k8s manifests.** A camp that cannot run a Gas City pack is not a lighter Gas City; it is a different tool wearing the vocabulary. Packs and formulas *are* the process — without them camp is a daemon with a ledger.

Today camp and Gas City are mutually unusable, in both directions:

- **A fresh camp knows zero agents** (#80). `camp init` writes `camp.toml`, `camp.db`, a gitignore entry, and nothing else.
- **Pack layering was half-built and the code admits it.** `pack.rs:175` pushes `<pack>/agents` and *nothing else*. `orders/mod.rs:264` still carries the comment *"Phase 12's pack layering replaces this body"* — it never landed. `packs/starter/orders.toml` is read by **no code path**.
- **`camp export` produces packs Gas City silently discards** (#85). gc takes each *immediate subdirectory* of `agents/` as an agent and `continue`s past files (`agent_discovery.go:33`). Camp exports `agents/dev.md` — a file. Zero agents, no error, either side.

## 2. The evidence base

Measured against the public `gastownhall/gascity-packs` repo (100 formulas, 80 agents, 11 packs) and Gas City's source at the ref this repo pins (`ci/gc-compat/GASCITY_REF`). The decisions are downstream of these numbers.

**The corpus is not uniform, and that decides the phasing:**

| pack | agents | needs long-lived sessions | mail refs |
|---|---|---|---|
| compound-engineering | 28 | no | **0** |
| gstack | 13 | no | **0** |
| bmad | 10 | no | **0** |
| gascity/roles | 12 | no | 6 |
| superpowers | 9 | no | 2 |
| **gastown** | 7 | **yes** — 5 `[[named_session]]` | **46** |
| oversight-rig | 1 | yes | 2 |

**51 agents across three packs need neither long-lived sessions nor mail.** They are bead-scoped workers — exactly camp's model. gastown is the outlier, and it is the only pack that forces the standing-session question.

**Formulas — 79 of 100 declare `contract = "graph.v2"`**: a DAG of steps with dependencies. That is the model campd *already implements* (`dispatch.rs:959-976` — "campd as purely mechanical control dispatcher for check loops, retry classification, on_complete fan-out, and run finalization").

**Control plane — camp already has 6 of gc's 8 control kinds.** gc's `ControlKinds` = `{retry, ralph, check, retry-eval, fanout, drain, scope-check, workflow-finalize}` (`beadmeta/kindsets.go:35-44`). Camp's `GraphRuntime` covers check/ralph (`PendingCheck`), retry/retry-eval, fanout (`PendingFanout`), and workflow-finalize. Missing: **`drain`** (13/100) and **`scope-check`** (**1**/100).

gc *externalizes* its control plane as an agent process (`gc convoy control --serve`); camp *internalizes* it in the daemon. Same job, different placement. **A vocabulary gap, not a missing subsystem** — therefore a compiler concern, not a runtime verb. Independently confirmed in review, which found camp's parser already accepts gc's `RalphSpec` shape (`{max_attempts, check{mode,path,timeout}}`) byte-for-byte.

**Agents — 80 real ones:** all are a directory + prompt + `agent.toml`; 72/80 use only `scope` + `description` + `fallback` + the prompt. Pooled: **2**. `pre_start`: **2**. `work_dir`: **8**. **Declaring a model, permission mode, or tool allowlist: 0** — see §5.2, this is a security decision, not a detail.

**Prompts — all 80 contain Go-template syntax**; 67 need template *actions* (`{{template}}`, `if`, `range`). Fragment includes: 84 uses. None execute shell (`cmd` returns the binary's own name, `prompt.go:394`).

**The worker contract — prompts hardcode `gc` and `bd`.** This killed rev 1's design. Measured over the prompt corpus:

| verb | literal `gc <verb>` | `{{cmd}} <verb>` |
|---|---|---|
| `hook` | **140** | **0** |
| `bd` | **152** | **0** |
| `runtime` | 129 | 3 |
| `prime` | 0 | 8 |
| `mail` | 51 | 7 |

`{{cmd}}` abstracts `prime` and `mail` — and **none** of `hook`, `runtime`, or `bd`. Rev 1 assumed the opposite and would have shipped workers that die `command not found` on their first line.

**Mail — gc's messages ARE beads** (`beadmail.go:198-207`, `Type="message"`, `Assignee=to`). Camp already has a `mail` bead type (`fold.rs:13`), already excluded from dispatch (`readiness.rs:84-88`), already exported as bd's `message` (`export.rs:189`). And the packs' own doctrine is **nudge-first**: *"Every `gc mail send` creates a permanent bead… Default to nudge for all routine communication. The litmus test: if the recipient dies and restarts, do they need this message?"*

## 3. Goal, and what v1 actually requires

**v1:** `camp import add https://github.com/gastownhall/gascity-packs/tree/main/bmad --name bmad` fetches a real Gas City pack and its agents, formulas, and orders run in camp **without editing the pack**. Same for `gstack` and `compound-engineering`.

**Rev 2 chose those three packs by reading only their agents, and was wrong.** Read at the `pack.toml` and formula layers, they demand three things rev 2 put out of scope or in a later phase:

| what | evidence |
|---|---|
| **A transitive pack import.** All three declare `[imports.gc] source = "../gascity"`. **24 of their 32 formulas `extends` a parent (`build-base`, `implement`, `do-work-item`…) that exists only in the gascity pack.** Without resolving it, the three packs contribute **zero runnable formulas**. | measured, `tomllib` |
| **`drain`.** Each pack's headline entry formula (`bmad-build`, `gstack-build`, `compound-build`) has **two `drain` steps**, both `member_access = "exclusive"`. **The step that does the work *is* a drain step, in all three.** | measured |
| **A worker environment.** All 51 agents share one fragment that reads `BEADS_ACTOR` / `GC_SESSION_NAME` / `GC_AGENT` / `GC_TEMPLATE` and **exits 0 doing nothing** if they are unset. Camp sets only `CAMP_*`. | `spawn.rs:230` |

So v1 is redefined by the corpus, not by preference:

- **Single-level pack imports are IN SCOPE, in phase 1.** Recursion depth **1 covers 100% of the corpus** — no pack imports a pack that itself imports. A relative `../gascity` source must resolve *after* materialization (see §7).
- **`drain` moves to phase 2**, with formulas. There is no v1 without it.
- **The v1 unit is `bmad | gstack | compound-engineering` + `gascity`**, imported together.
- **`gascity` carries 10 `mail` calls**, so v1 either implements mail or states plainly what those agents lose. **Decision:** `camp mail` ships in v1 (§8.2), not v1.5 — the transitive dependency dragged it forward and pretending otherwise is how rev 2 broke.

**Not the goal:** being Gas City. Camp stays single-binary, SQLite-ledgered, zero-idle-cost, daily-driver scale. Where fidelity and camp's laws conflict, camp **refuses loudly and names what it refused** — never silently approximates.

**gastown is still v2.** It needs standing named sessions, pools, and 66 mail calls. That is where invariant 2 gets challenged, and isolating it keeps it from contaminating the rest.

## 4. The permissiveness rule

Gas City parses formulas with plain `toml.Unmarshal` — **no unknown-field check** (`parser.go:233`); its spec says *"Unknown top-level keys are silently ignored."* The corpus is full of keys the engine never implemented:

| key | formulas | in gc's struct? |
|---|---|---|
| `version` | 93 | **no** |
| `target_required` | 64 | **no** |
| `internal` | 40 | **no** |
| top-level `mode` | 7 | **no** |
| top-level `single_lane` | 6 | **no** |
| `sling_container_mode` | 1 | **no** |

Same for agents: **`fallback` is set by 72 of 80 and is not a field in gc's `Agent` struct.** It was a name-collision tiebreaker, removed; the spec says a stale `fallback` key "is ignored".

**93 of 100 formulas name at least one dead key** — including Gas City's own, which Gas City runs. **A consumer that refuses unknown keys is stricter than the reference implementation and rejects packs that work.**

The rule:

1. Semantics camp does not implement → **refuse the formula, naming the key.**
2. No semantics in Gas City → **ignore, warn once** (a future gc that gives it meaning surfaces as divergence, not drift).
3. Pure annotation (`notes`, `catalog`, formula-level `metadata`) → ignore silently.

**Three traps, all mandatory:**

- **Position-overloaded keys.** Top-level `mode`/`single_lane` are dead; **`[steps.check.check].mode = "exec"`** (49 uses) and **`[steps.drain.item].single_lane`** are load-bearing. Key off *nesting*, never name.
- **`target_required` looks semantic and is not.** gc derives "needs a target" *structurally*. Reading the key diverges wherever they disagree.
- **Step `metadata` is NOT an annotation.** It carries **`gc.run_target`** (53 formulas) — *routing intent*. Ignore it and work silently goes to the wrong agent. Rev 1 mis-classified this.

**Camp's `agent.toml` parser must tolerate unknown keys** (it already does — `pack.rs:329`) or 72/80 agents hard-fail. `camp.toml`'s `deny_unknown_fields` strictness **must never leak into `agent.toml`**. Pin with a regression test using `fallback = true`.

## 5. Agents

### 5.1 One format: the Gas City directory. The Claude Code `.md` file is retired.

```
agents/<name>/
  agent.toml            optional (agent_discovery.go:47)
  prompt.template.md    canonical; or prompt.md.tmpl; or prompt.md
  namepool.txt          optional
```

Identity is the **directory name** (`agent_discovery.go:51`). gc **silently ignores unknown `agent.toml` keys** — it decodes with `toml.Decode` and discards the metadata (`:48`), and the fatal-unknown-key check guards `pack.toml` only. So camp's own keys ride along and the pack stays valid in a city.

**#85 disappears by construction:** if camp's native format *is* gc's, `camp export` copies `agents/` verbatim. No translation ⇒ no translation bug.

Camp renders a defined Go-template subset: `{{.Var}}`, `{{template "name"}}` with gc's fragment precedence, `if`/`range`, and `cmd`/`basename`/`session`/`templateFirst`. **`{{cmd}}` renders `camp`.** Camp does **not** copy gc's fail-soft template behaviour (`pool.go:227-237` keeps the raw string on error — a silent-corruption path); camp **fails fast**, and a prompt using an unsupported construct is a **refused pack**.

### 5.2 Where model, permission mode, and tools come from — and the hole this closes

**Zero of 80 gc agents declare a model, permission mode, or tool allowlist.** In Gas City these come from the **provider profile** — a layer camp does not have — and gc's claude profile defaults to **`permission_mode = "unrestricted"`**, which maps to `--dangerously-skip-permissions` (`profiles.go:102-176`). Worse: **there is no `tools` option key in any of gc's 17 providers.** A gc pack *cannot express a tool allowlist at all*.

Camp's `spawn.rs:198-208` pushes `--model` / `--permission-mode` / `--allowedTools` only `if let Some(...)`. So naïvely importing a pack would spawn **downloaded agents as bare `claude -p` with no tool restriction** — a hole larger than the one `trust_exec` closes, and camp would be inheriting gc's unrestricted default by accident.

**Decision:**

- Camp gains **`[agent_defaults]` in `camp.toml`** — *operator-owned*, never pack-owned: `model`, `permission_mode`, `tools`.
- **A pack cannot influence model, permission mode, or tools at all.** Rev 2 allowed `option_defaults` to "narrow" them; that clause is **deleted**. `option_defaults` appears **0 times in all 80 real agents**, so it was a rule for nobody — and "narrow" is undefined for an ordered enum (`permission_mode`) and for a set (`tools`: intersection? error on an un-granted request? silent drop?), which is exactly the ambiguity two implementers resolve differently. Operator-owned, full stop: simpler, strictly safer, and it costs zero corpus compatibility.
- **Camp refuses to spawn any agent for which no tool allowlist resolves.** Unrestricted-by-omission is impossible.
- Camp **never** adopts gc's `unrestricted` default.

This is operator configuration, not a fallback: absent config is an *error*, not a default.

### 5.3 `skills/` is NOT ignorable — 13 of the 51 v1 agents depend on it

Rev 2's component spec said `skills/` and `commands/` are *"IGNORED by camp. That is a design decision, not an oversight."* It was an oversight.

**9 of bmad's 10 agents** and **4 of compound-engineering's 28** open with, verbatim: *"Use the shared `bmad-create-architecture` skill from this pack's `skills/` catalog."* Ignore `skills/` and 90% of bmad's agents are pointed at instructions that do not exist — so they improvise, silently, which is worse than failing.

**Camp installs a pack's `skills/` into the worker's `.claude/skills/`.** `commands/` (Claude Code slash commands) remain out of scope and are **reported as ignored** at import.

### 5.4 What camp refuses (v1), loudly and itemized

`pre_start` (2/80) · `work_dir` (8/80) · `wake_mode`/`idle_timeout` (7/80) · `min_active_sessions` (2/80) · `max_active_sessions` (**7**/80) · `nudge` (6/80) · `sleep_after_idle` / `max_session_age` / `max_session_age_jitter` (1 each) · `[[named_session]]` · `[global].session_live` (tmux chrome only) · `tmux_alias` (0 uses) · ACP (0 uses) · provider presets (0 `[providers.*]` in the whole corpus).

Every refusal names the pack, the agent, and the key, **and appends a ledger event**. Never silently skipped.

**Warnings are aggregated, not per-key.** 93 of 100 formulas name at least one dead key (§4); a warning per key is a wall of noise nobody reads. `camp import add` prints **one line per import naming the distinct ignored keys**, and appends one ledger event.

## 6. The worker contract: camp serves gc's vocabulary

Prompts are 140-line bash blocks with inline `python3` JSON parsers. **Rewriting them at import is not realistic** and forks every pack. So camp serves the vocabulary — and since prompts hardcode `gc` and `bd`, camp puts both on the worker's PATH.

**The shims are argv translators, not a second store client:**

```sh
# .camp/bin/gc                    # .camp/bin/bd
#!/bin/sh                         #!/bin/sh
exec camp gc-shim "$@"            exec camp bd-shim "$@"
```

campd writes these into `.camp/bin/` and prepends that directory **to the worker's PATH only** — the env camp already controls. **`camp` remains the sole process that touches the ledger.** An operator's own `gc`/`bd` (if they also run a city) is untouched, because camp sets the child's env, not the shell's.

**Verbs camp must serve:**

| verb | refs | contract |
|---|---|---|
| `hook --claim --json` | 154 | discovery **+ atomic claim**. `{schema_version, ok, action:"work"\|"drain", reason, bead_id, assignee, route}`. Exit 0 on work; **exit 1 on drain** unless `--drain-ack` → 0. Route on `gc.routed_to` / `gc.run_target`. Camp's `camp claim` is only the final flip — discovery, routing, `gc.work_branch` stamping and session pointers are new. |
| `bd show/update/close/list/ready/create` | 308+ | the worker data plane. `--json`, `--set-metadata k=v`. Camp's outcome vocabulary already matches gc's (`pass`/`fail`; `shipped`/`no-op`/`blocked`/`abandoned`, `vocab.rs:47,57`) — pinned to it on purpose. |
| `runtime drain-ack` | 185 | the session-exit handshake. **A gc worker may not legally exit without it.** |
| `prime` | 31 | render the agent's prompt template to stdout |
| `mail` (v1.5) | 72 | §8 |
| `convoy status --json` | 9 | worker-facing read |

**Unknown subcommands and flags FAIL FAST**, naming exactly what the pack asked for and which pack asked. Never a no-op — a silently-ignored `bd update --set-metadata gc.outcome=pass` is a corrupted ledger.

**But failing fast is not enough, because the caller swallows failures.** The worker fragment opens its claim block with `set +e` and routes every failure into `sleep 2; continue` inside an **unbounded `while true` loop**. A refused `bd` call there does not surface as a fast failure — it produces a silent spin. So **every shim refusal also appends a ledger event** (`shim.refused`, naming pack / agent / verb / flag). The operator sees it even when the bash block eats it. Without that, "fail fast" is a claim the architecture cannot honour.

*(Scale note: within the 51 v1 agents, `bd mol` / `bd ready` / `bd gate` appear **only as prohibitions** in the shared fragment — "do not run broad `bd ready`". Refusing them is safe for v1. The corpus-wide counts rev 2 quoted were dominated by those negations.)*

### 6.1 The worker environment contract — without it, all 51 agents no-op

Every v1 agent's first act is this, from the shared `gc-role-worker` fragment:

```sh
EXPECTED_ASSIGNEE="${BEADS_ACTOR:-${GC_SESSION_NAME:-${GC_SESSION_ID:-${GC_AGENT:-}}}}"
EXPECTED_ROUTE="${GC_TEMPLATE:-${GC_AGENT:-}}"
if [ -z "$EXPECTED_ASSIGNEE" ]; then echo "CONFIG_REJECTED missing expected assignee"; gc runtime drain-ack; exit 0; fi
if ! command -v python3 >/dev/null 2>&1; then echo "CONFIG_REJECTED missing python3"; gc runtime drain-ack; exit 0; fi
```

Gas City exports those (`build_desired_state.go:1001-1003`, `bd_env.go:229-230`). Camp exports only `CAMP_DIR`, `CAMP_BEAD`, `CAMP_SESSION`, `CAMP_TRANSCRIPT` (`spawn.rs:230`). **Every one of the 51 agents would print `CONFIG_REJECTED` and exit 0** — a clean exit, for a worker that did nothing. That is rev 1's "dies on line one" failure reproduced through a different hole.

**Camp exports the gc worker environment**, and the values must **equal by construction** what `camp hook --claim --json` returns:

| var | value |
|---|---|
| `BEADS_ACTOR`, `GC_SESSION_NAME`, `GC_SESSION_ID` | the session name — **the same string `hook --claim` returns as `assignee`** |
| `GC_AGENT`, `GC_TEMPLATE` | the agent's qualified name — **the same string returned as `route`** |

This is not cosmetic. The fragment compares `EXPECTED_ASSIGNEE` against the bead's assignee and `EXPECTED_ROUTE` against `hook`'s `route`, and **a mismatch does not fail — it `sleep 2; continue`s forever.** If camp's exported values and its `hook` output disagree by a byte, the worker spins in one Bash call until it is killed.

**`python3` is a hard runtime dependency of the gc worker contract.** It must be declared, and added to `contrib/docker/` (which installs only `ca-certificates git tini`).

### 6.2 Session lifecycle: camp truncates gc's continuation loop, and says so

The gc worker is **not bead-scoped**. After closing a bead it loops — *"check for more routed work before draining… continue by running the same `GC_CLAIM` block again"* — and exits only via `gc runtime drain-ack`.

Camp is bead-scoped: on `bd close`, campd drops the held stdin and kills the worker after a grace (`dispatch.rs:243-269`). Two outcomes, both wrong: the worker is killed before it can `drain-ack` (violating the handshake §6 calls mandatory), or it wins the race and **claims a second bead that campd's registry does not know about** — a bead then claimed by a session campd is about to kill.

**Decision:** `camp hook --claim --json` returns **`action: "drain"`** for any session whose dispatched bead is already closed. One bead per session is enforced **at the hook**, so the worker takes its own clean `NO_ROUTED_WORK → drain-ack → exit 0` path. **`drain-ack` becomes campd's release signal** (release on drain-ack, with the existing grace timer as a backstop), not bead-close.

**Fidelity cost, stated plainly:** camp does not honour `gc.continuation_group`, and a `context = "shared"` drain does not actually share a worker session. Master §8.4 is therefore **narrowed, not upheld** — see §11.5.

### 6.3 The shims: absolute path, gitignored, dispatch-only

- **The shim must embed camp's absolute path** (`std::env::current_exe()`), not `exec camp …` by bare name. campd's PATH is a snapshot baked into the service unit at install (`service/mod.rs`, `campd_path()`), and it is not guaranteed to contain camp's own bindir — that snapshot exists so campd can find *`claude` and `git`*. A bare-name lookup would re-introduce the exact PATH bug this repo just spent five review rounds fixing.
- **`.camp/bin` is gitignored.** `gitignore::RUNTIME_DIRS` gains `bin` alongside `imports`. Autonomous workers deliver by commit from a worktree; generated executables must not ride along.
- **Attended sessions get no shims.** Camp sets the *worker child's* env, not the operator's shell. **Therefore gc pack agents are campd-dispatch-only and cannot be run attended** — stated so nobody wires `.camp/bin` into the plugin's SessionStart hook.
- The shims **shadow a real `gc`/`bd`** inside a worker for an operator who also runs a city. That is intended (the worker must talk to camp's ledger), and it is why refusals must be loud.

## 7. Import machinery

Per the component spec, retained: `[imports.<name>]` in `camp.toml`; a tracked `packs.lock` (`schema = 1`, keyed by the **verbatim source string**, `{version, commit, fetched}` — gc's shape); materialization into `.camp/imports/<name>/` (gitignored); gc's source grammar verbatim; `camp import add | install | list | remove | upgrade | check`.

`pack.toml` **required** with `[pack].name` + `[pack].schema` (≤2) — gc's rule (`pack.go:2372-2391`). **`version` is not required** (gastown ships without it; gc's generated JSON schema wrongly says otherwise).

**Symlinks are dereferenced on materialization** — `packs/starter/formulas/guarded-change.toml` symlinks *outside* the pack subpath; materializing the subpath alone leaves a dangling link.

## 8. Sessions, control, and comms

### 8.1 Workers stay headless. The control plane is a protocol.

> **This section is a summary. The full design is `2026-07-12-camp-control-plane-design.md`** — the operator's view, the permission flow, the overseer, and the undocumented-protocol risk (accepted, with mitigations).

Camp spawns `claude -p --output-format stream-json --input-format stream-json` and holds stdin as a live pipe. That channel carries a **full control protocol** — verified in the shipped binary (v2.1.207): `control_request` / `control_response` / `control_cancel_request`, and the subtypes **`interrupt`**, **`can_use_tool`**, **`set_model`**, **`set_permission_mode`**.

So camp can watch, converse with, **interrupt**, and **answer permission requests for** a worker **without a PTY, tmux, or screen-scraping**. Gas City needs tmux because it drives agents through a terminal; herdr regexes the rendered TUI because it only has pixels. Camp kept the structured channel and can simply *use* it.

**campd's socket is the control plane, and it is the only path to a worker:**

`sessions.list` · `session.subscribe` (live typed event stream) · `session.send_turn` · `session.interrupt` · `session.permission_decision` · `session.set_model` / `set_permission_mode`

**Every client goes through it — no exceptions.** `camp attach` (a TUI) is the first client. The `camp:operator` skill (an overseer agent that can watch and interrupt other agents) is the second. A future API/Web UI is the third.

**Two constraints, honoured now, costing nothing:**
- The protocol addresses sessions by **name/id**, never by pid or file path. A worker that later lives in a VM on another host must not break every client.
- **campd owns the truth; clients are stateless renderers.** A TUI that tails files directly works until the files are on another machine.

*(A remote API, a Web UI, and per-agent VM isolation are explicitly **out of scope**. They are named only so this design does not foreclose them.)*

### 8.2 Comms: nudge first, mail second — and mail needs a recipient

The packs' own doctrine: *"Default to nudge for all routine communication… If the recipient dies and restarts, do they need this message? If yes → mail. If no → nudge."*

Camp **already has nudge** — a turn into the live worker's held stdin, or `claude --resume` once it has exited. The gap is a nudge to a session that **does not exist yet** (queue it, materialize on spawn).

**Mail lands in v1.5, on the bead type camp already has.** `type = "mail"`, plus a `from` field, a `read` marker and a `thread:<id>` marker (labels are the carrier — exactly what gc does). Verbs: `send`, `inbox`, `check`, `read`, `peek`, `reply`, `archive`, `count` — with gc's flag spelling and gc's exit-code contract on bare `check` (**0 = has mail, 1 = empty**; a status line depends on it).

Three things that will bite an implementer who doesn't read this:
- **Delivery is pull + inject.** An agent learns it has mail because a **`UserPromptSubmit` hook** runs `camp mail check --inject` each turn. Camp's plugin wires only SessionStart/SessionEnd today — **without the new hook, mail is write-only.**
- **`gc mail send mayor/`** — the packs literally write a trailing slash. Trim it, or gastown's escalation path silently 404s.
- **Sanitize sender/subject/body against a `</system-reminder>` breakout.** Injected mail is untrusted text entering the model's context. Gas City learned this the hard way.

**Invariant 1 is narrowed here, deliberately and on the record.** AGENTS.md says *"No ticks, no polling loops, anywhere."* A per-turn `mail check` is a poll — it burns tokens rather than CPU, and it rides the worker's own turn boundary rather than a timer. **campd itself still never polls.** This is a real amendment to the decision record, not a reading of it (§11).

### 8.3 Standing sessions are v2, with gastown

`[[named_session]] mode = "always"` exists so *there is somebody to receive mail*. Camp's worker dies with its bead, so today **a camp mailbox has no recipient**. That is the whole of the gastown gap, and `min_active_sessions > 0` is a standing process with no bead — **a direct violation of invariant 2**.

v2 designs it; v1 and v1.5 do not need it (0 named sessions across 51 agents, 8 mail refs across 72).

## 9. Formulas

**The ladder is a TEST, not a claim.** Three independent attempts to compute it produced three different answers (48/75/87/100, then 34/61/87/97, then 32/59/85/98) — the discrepancies were all in *which rung owns which key*, most damningly step `metadata`, which is routing and not annotation. A number nobody can reproduce is a boast. So:

**The compatibility gate (§10) asserts the exact count at each phase, per key set.** The spec commits to the *shape*, not a headline:

| phase | key set added | corpus loading |
|---|---|---|
| 2a | dead keys ignored; annotations; `contract`; `description_file` (**53** formulas); step `metadata` (incl. `gc.run_target` routing) | pinned by test |
| 2b | `vars`, `condition` (**13**) | pinned by test |
| 2c | `extends` (48) | pinned by test |
| 2d | `type`, `template`, `expand`, `expand_vars`, `children` | pinned by test |
| **2e** | **`drain` (13)** — moved here from phase 5. **v1's three packs cannot run without it**: their headline formula's `implement` step *is* a drain step. | pinned by test |

**Permanently refused, and therefore the ceiling is below 100:** `phase = "vapor"` (2 formulas — v1 materialization semantics) and `scope-check` / `gc.scope_*` (1). Rev 1 claimed 100/100 while simultaneously refusing these. **The ceiling is 97–98, and the gate will say exactly which.**

Semantics an implementer must get right (each verified in gc's compiler):

- **`description_file` (67 formulas)** — the file's *contents replace the step description* at parse time, and those steps typically carry **no inline description**. Ignore it and the worker gets **zero instructions**. `../assets/…` resolves **through the formula layers** (highest wins — that's how a pack shadows prose while inheriting structure). >4096 bytes ⇒ a pointer prompt, so the *path* must still resolve.
- **`extends` (48)** — child seeds scalars; parents' steps **append**; a child step whose `id` matches a parent's **replaces it whole, in place, preserving position**. No field-level merge. `advice`/`pointcuts` are dropped entirely.
- **`condition` (17)** — `==` and `!=` only; LHS must be a single `{{var}}`. **False ⇒ the step is PRUNED with its children**, and dangling `needs` edges are silently dropped.
- **`{{var}}` substitution** — applies to `title`, `description`, `assignee`, metadata values, `notes`, tags. **Not** to `id`, `needs`, `check.path`, or `drain.formula`. An undefined var **keeps the literal placeholder**; the residual check is enforced **only on `title`**. Reproduce that asymmetry or diverge.
- **`type = "expansion"`** — the formula is **not directly runnable**; it supplies `template` steps for `expand`.
- **`drain` (13, phase 5)** — runtime fan-out. **`member_access = "exclusive"` (25 uses)** writes a per-member reservation and **fails if another drain owns it**. Ignore it and two drains concurrently mutate one bead: silent data corruption.
- **21 formulas declare no `contract` at all** — they are not `graph.v2` in gc. Camp must **not** run them under graph.v2 semantics by default. Refuse, or state the fidelity risk explicitly; do not silently assume.

## 10. The compatibility gate

Today's CI proves **camp ⊆ gc** (camp's corpus compiles under the real gc compiler). That is **the wrong direction** and proves nothing about running gc packs.

The new gate: **fetch the real `gascity-packs` corpus at a pinned ref and assert camp loads and compiles exactly what it claims — and refuses, by name, what it does not.** The claimed numbers become tests. A regression from 85 to 60 fails CI.

**Do not vendor the corpus.** `gastownhall/gascity-packs` has **no top-level LICENSE**; it is a mixed-license tree (third-party vendored dirs carry their own), i.e. all-rights-reserved by default, and gascamp is AGPL-3.0. The repo ships **`registry.toml` with per-pack `commit` + `sha256:` hashes** (11 packs, 28 releases) — **fetch and verify against those pins at CI time.**

**Two things this obliges, which "verify the sha256" hides:**
- The hash is **not a tarball digest**. It is a bespoke *manifest* hash (`validate_registry.py:94-136`): for every file in the pack subtree at that commit, build `"<relpath> <perm> <sha256(content)>"` (perm from the git mode — `100644`→`644`, `100755`→`755`, **`120000`→symlink**), join, and sha256 the result. Camp's gate must **port that algorithm**, and pin the version of it, because it is a third party's undocumented, unversioned function.
- **Verification runs on the raw fetched tree, BEFORE symlink dereference** (§7). Dereferencing turns mode `120000` into `100644` and invalidates the hash.

The gate must also state **which `[[pack.release]]` version it pins**.

Both gates run. Without the new one, "compatible" is a claim, not a fact.

## 11. Decision-record amendments (this section IS the spec PR)

AGENTS.md forbids re-litigating the record without one. Four changes:

1. **Master §11 — "a role is a Claude Code agent file, zero invented formats."** *Retired.* Camp's native agent is a Gas City directory (§5.1). The trade is deliberate: gc compatibility is worth more than bare-Claude-Code portability, because packs+formulas *are* the process. (The companion clause — *machinery ships no roles* — **survives**: camp's binary carries only a pack *source URL*. Note gc's own machinery ships one agent, `core/control-dispatcher`, `prompt_mode="none"` — so the narrow reading is what holds in gc too.)
2. **Invariant 1 — "no polling loops, anywhere."** *Narrowed:* campd never polls; a per-turn `mail check` hook in the **worker** is a poll that costs tokens, not idle CPU (§8.2).
3. **Invariant 5 — "no fallbacks."** *Upheld, and made sharper:* a pack agent with no resolvable tool allowlist is an **error**, not a default (§5.2). Camp never inherits gc's `unrestricted`.
4. **Master §8.4 — workers spawn per bead and exit on close.** *Narrowed, not upheld* (rev 2 claimed "upheld" and was wrong). The gc worker contract camp adopts mandates a **multi-bead continuation loop** and a **`drain-ack` exit handshake**; camp is bead-scoped and kills the worker on close. Camp therefore **truncates gc's session protocol**: `hook --claim` returns `drain` once the session's bead is closed, and `drain-ack` becomes the release signal (§6.2). **Fidelity cost:** `gc.continuation_group` is not honoured, and a `context = "shared"` drain does not share a worker session. Standing named sessions (which gastown requires) remain **v2**, and that phase must re-litigate **invariant 2** explicitly (§8.3).
5. **Master §11's `skills/` corollary.** Retiring "a role is a Claude Code agent file" (amendment 1) does not license dropping a pack's `skills/`: **13 of the 51 v1 agents' prompts depend on it** (§5.3). Camp installs pack skills into the worker's `.claude/skills/`. A format decision must not silently become a content decision.

## 12. Phases

1. **Import machinery + pack loader.** Fetch/lock/install, git hardening, `trust_exec`. `pack.toml`, **single-level `[imports.*]`** (§3 — depth 1 covers the whole corpus), agent directories, `formulas/`, `orders/` **directories**, `skills/` install. Fixes #80; retires the `.md` agent format; fixes #85 by construction.
2. **Formulas** (§9), phase-gated by key set, each rung pinned by the gate — **including `drain` (2e)**, without which none of v1's packs run.
3. **The worker contract** — `gc`/`bd` shims (§6.3), the **worker environment** (§6.1), `hook --claim --json`, `runtime drain-ack` as the release signal (§6.2), `python3` in the container, **and #86 (`--verbose`)**. A gc worker closes a gc bead end-to-end.
   **v1 target: `bmad | gstack | compound-engineering` + `gascity` (the transitive dependency).**
4. **Mail** (§8.2) + `prime`. Pulled into v1 by gascity's 10 mail calls, not deferred.
5. **The control plane** — see `2026-07-12-camp-control-plane-design.md`, which has its own phase 0 (the read channel).
6. **gastown** — standing sessions, pools, 66 mail calls. The phase that re-opens invariant 2.

## 13. Security

**Fetch.** Camp's first production git subprocess. gc's untrusted-remote hardening ported verbatim (`internal/git/git.go:385-395`): `http.followRedirects=false`, `protocol.allow=never` + an explicit `https/http/ssh/git/file` allowlist (this is what blocks `ext::` — arbitrary command execution), `core.hooksPath=/dev/null`, `core.fsmonitor=false`, `core.untrackedCache=false`, sanitized `GIT_*` env. One helper owns the flags; a test asserts the argv byte-for-byte. The threat model is **not** "the operator typed the URL" — `camp.toml` and `packs.lock` are **tracked**, so a source arrives via `git pull`, a PR branch, or CI.

**Execute — `trust_exec`, default deny.** `[imports.<name>] trust_exec = true` gates a formula's `check.path`, `pre_start`, `exec` orders, and `condition` shell. `camp import add` **inventories the executable content** and prints the command to enable it. This closes a hole that was about to open silently: `check.path` scripts are operator-authored today and become pack-authored the moment pack formulas load.

**Tools.** §5.2. No agent runs without a resolved allowlist.

**The money invariant.** An order fires a formula; a formula dispatches workers; workers cost real money.

> **Nothing an import brings may fire until the operator names it in `[orders] enabled`.**

Imported orders load, validate, and appear in `camp order ls` as **disabled**, with their source.

## 14. Testing

- **No network.** Git-backed imports run against local `file://` repos in a temp dir — the real clone/lock/materialize path.
- **No API spend.** Workers are `#!/bin/sh` fakes. Never a real `claude`.
- **The money invariant gets a test that can fail:** an imported pack with a due cron order fires **nothing** until `[orders] enabled` names it.
- **`trust_exec` likewise:** a pack formula carrying a `check.path` executes nothing untrusted.
- **The tool-allowlist refusal likewise:** an agent with no resolvable `tools` must not spawn.
- **`fallback = true` must parse and be ignored** (72/80 real agents depend on it).
- The git hardening argv is asserted byte-for-byte. A dropped flag is a removed fence.
- Every new test must die against a mutation of the code it guards.

## 15. Out of scope (#84)

`[[exports]]`/namespaces, semver constraint solving, the pack registry, a shared machine-local cache, credentials for private pack repos, `why`/`--tree`/`prune`/`status`/`migrate`, gc's `overlay/` mutation semantics, `commands/`, `doctor/` checks, ACP, provider presets, tmux, and a remote API / Web UI / per-agent VM isolation (the control-plane spec constrains the design so these stay possible; it does not build them).

**No longer out of scope:** the **transitive import graph**. Rev 2 listed it here while its own goal statement required it — all three v1 packs declare `[imports.gc] source = "../gascity"` (§3). **Single-level resolution is in phase 1**; depth >1 does not occur in the corpus and stays out.

## 16. Corrections to rev 2's numbers

Re-derived and confirmed by review. Rev 2 was wrong on these, and they are fixed above:

| claim | rev 2 | actual |
|---|---|---|
| `description_file` | 67 formulas | **53** |
| `condition` | 17 | **13** |
| `max_active_sessions` | 2 | **7** |
| gastown mail calls | 46 | **66** |
| literal `gc hook` refs | 140 | **151** (the load-bearing half — `{{cmd}} hook` = **0** — was right) |
| `bd mol` / `bd ready` | "102 / 80 refs, no camp equivalent" | corpus-wide counts **dominated by prohibitions**; **0 real invocations** among the 51 v1 agents |

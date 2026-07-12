# Gas City pack compatibility — design

**Date:** 2026-07-12
**Issues:** [#80](https://github.com/Liquescent-Development/gascamp/issues/80) · [#84](https://github.com/Liquescent-Development/gascamp/issues/84) · [#85](https://github.com/Liquescent-Development/gascamp/issues/85)
**Incorporates:** `2026-07-12-camp-pack-imports-design.md` — the import machinery (source grammar, lock, verbs, `camp init` flow, error table) in full detail. That is the component spec for §7; this is the umbrella. Where they disagree, this wins.
**Status:** rev 4 — after a third adversarial review (findings recorded in `2026-07-12-KNOWN-DEFECTS.md`). Rev 3's central gap: camp resolves agents by bare name while **every route in the corpus is `<binding>.<agent>`** — one missing namespace produced four of the five Criticals (routing, the day-one collision, the "nested pack", an unamended layering law). Rev 4 makes the import binding first-class (§7.1) and re-grounds the fix in how gc actually deploys this corpus: **gc does not auto-discover `gascity/roles`; the packs' own READMEs instruct an explicit rig-scoped import bound as `gc`** — so camp needs a namespace and a recipe, not new discovery machinery. Every number below is reproducible via `ci/gc-compat/measure_corpus.py`.

## 1. Why

README: camp is *"what k3s is to k8s"* for Gas City. **k3s runs k8s manifests.** A camp that cannot run a Gas City pack is not a lighter Gas City; it is a different tool wearing the vocabulary. Packs and formulas *are* the process — without them camp is a daemon with a ledger.

Today camp and Gas City are mutually unusable, in both directions:

- **A fresh camp knows zero agents** (#80). `camp init` writes `camp.toml`, `camp.db`, a gitignore entry, and nothing else.
- **Pack layering was half-built and the code admits it.** `pack.rs:175` pushes `<pack>/agents` and *nothing else*. `orders/mod.rs:264` still carries the comment *"Phase 12's pack layering replaces this body"* — it never landed. `packs/starter/orders.toml` is read by **no code path**.
- **`camp export` produces packs Gas City silently discards** (#85). gc takes each *immediate subdirectory* of `agents/` as an agent and `continue`s past files (`agent_discovery.go:33`). Camp exports `agents/dev.md` — a file. Zero agents, no error, either side.

## 2. The evidence base

Measured against the public `gastownhall/gascity-packs` repo (100 formulas, 80 agents, 11 packs) and Gas City's source at the ref this repo pins (`ci/gc-compat/GASCITY_REF`). The decisions are downstream of these numbers, and the numbers are downstream of `ci/gc-compat/measure_corpus.py` — run it before trusting any of them.

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

**Routing — every route in the corpus is a qualified `<binding>.<agent>` name.** `measure_corpus.py` derives it two ways: raw, the top `gc.run_target` values are `gc.run-operator` (55) and `{{implementation_target}}` (46 — a formula var); resolved through each formula's `[vars]` defaults, those 46 become `superpowers.implementer` (16), `gc.implementation-worker` (10), `bmad.story-implementer` (8), `compound-engineering.ce-work` (6), `gstack.implementer` (6)… — **zero route values are bare names** (the 4 with no default receive a qualified value via `expand_vars`). gc stamps the binding onto every agent (`pack.go:339`, `agents[i].BindingName = bindingName`); the directory is bare (`bmad/agents/architect`), the routable identity is not (`bmad.architect`). §7.1 is the consequence.

**Prompts — all 80 contain Go-template syntax**; 67 need template *actions* (`{{template}}`, `if`, `range`). Fragment includes: 84 uses. None execute shell (`cmd` returns the binary's own name, `prompt.go:394`). Fragment, formula, and `extends` references are **bare names resolved through layers** (`{{ template "gc-role-worker" . }}`, `extends = ["build-base"]`); only agent routing is binding-qualified.

**The worker contract — prompts hardcode `gc` and `bd`.** This killed rev 1's design. Measured over the prompt corpus:

| verb | literal `gc <verb>` | `{{cmd}} <verb>` |
|---|---|---|
| `hook` | **151** | **0** |
| `bd` | **152** | **0** |
| `runtime` | 129 | 3 |
| `prime` | 0 | 8 |
| `mail` | 51 | 7 |

`{{cmd}}` abstracts `prime` and `mail` — and **none** of `hook`, `runtime`, or `bd`. Rev 1 assumed the opposite and would have shipped workers that die `command not found` on their first line.

**Mail — gc's messages ARE beads** (`beadmail.go:198-207`, `Type="message"`, `Assignee=to`). Camp already has a `mail` bead type (`fold.rs:13`), already excluded from dispatch (`readiness.rs:84-88`), already exported as bd's `message` (`export.rs:189`). And the packs' own doctrine is **nudge-first**: *"Every `gc mail send` creates a permanent bead… Default to nudge for all routine communication. The litmus test: if the recipient dies and restarts, do they need this message?"* **Every one of gascity's 10 mail references is `gc mail send human`** — a first-class gc mailbox addressed to the operator (`cmd_mail.go:863`); 6 live in workflow assets (`description_file` step prose), 4 in gc's own tests. **v1 has no agent-to-agent mail anywhere in its corpus** — §8.2 is sized accordingly.

## 3. Goal, and what v1 actually requires

**v1** is gc's own documented deployment of these packs, with camp as the consumer. The packs' READMEs (bmad step 1–2, gascity/README) prescribe **two imports**: the method pack, and the rig roles bound as `gc`:

```sh
camp import add https://github.com/gastownhall/gascity-packs/tree/main/bmad --name bmad
camp import add https://github.com/gastownhall/gascity-packs/tree/main/gascity/roles --name gc
```

After those two commands, bmad's agents, formulas, and orders run in camp **without editing the pack**. Same for `gstack` and `compound-engineering` (each substituting its own first line). The `[imports.gc] source = "../gascity"` each method pack declares is materialized by camp **automatically and transitively** (§7.2) — it supplies the formula parents, fragments, skills, and assets; it supplies **zero agents** (§7.3).

**Rev 2 chose those three packs by reading only their agents, and was wrong.** Read at the `pack.toml` and formula layers, they demand three things rev 2 put out of scope or in a later phase:

| what | evidence |
|---|---|
| **A transitive pack import.** All three declare `[imports.gc] source = "../gascity"`. **24 of their 32 formulas `extends` a parent (`build-base`, `implement`, `do-work-item`…) that exists only in the gascity pack.** Without resolving it, the three packs contribute **zero runnable formulas** — and lose their `[vars]` defaults too (`drain_policy = "separate"` is declared in `build-base`, not in the children). | measured, `tomllib` |
| **`drain`.** Each pack's headline entry formula (`bmad-build`, `gstack-build`, `compound-build`) has **two `drain` steps**, both `member_access = "exclusive"`. **The step that does the work *is* a drain step, in all three.** | measured |
| **A worker environment.** All 51 agents share one fragment that reads `BEADS_ACTOR` / `GC_SESSION_NAME` / `GC_AGENT` / `GC_TEMPLATE` and **exits 0 doing nothing** if they are unset. Camp sets only `CAMP_*`. | `spawn.rs:230` |

And rev 3 asserted mechanisms camp does not have. The corpus routes **only** to qualified names (§2), camp resolves **only** bare names, and rev 3 simultaneously demanded "the agent's qualified name" (§6.1) while putting namespaces out of scope (§15). Rev 4 closes that: **the import binding is a first-class namespace** (§7.1), and the v1 unit is:

- **`bmad | gstack | compound-engineering`**, imported by name;
- **`gascity`**, materialized transitively (content layers only);
- **`gascity/roles`, imported explicitly and bound as `gc`** — the corpus's own deployment, and the only place `gc.run-operator` and friends exist.

**Single-level pack imports are IN SCOPE, in phase 1.** Depth 1 covers 100% of the corpus — no pack's import target itself declares imports. **`drain` moves to phase 2.** **`camp mail` ships in v1** at the size the corpus actually uses: `send` to the operator's mailbox plus an operator-side inbox (§8.2) — not the agent-to-agent system rev 3 designed.

**Not the goal:** being Gas City. Camp stays single-binary, SQLite-ledgered, zero-idle-cost, daily-driver scale. Where fidelity and camp's laws conflict, camp **refuses loudly and names what it refused** — never silently approximates.

**gastown is still v2.** It needs standing named sessions, pools, and 66 mail calls — including the real agent-to-agent mail. That is where invariant 2 gets challenged, and isolating it keeps it from contaminating the rest.

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
- **Step `metadata` is NOT an annotation.** It carries **`gc.run_target`** (53 formulas) — *routing intent*, and every value of it is a qualified name (§2). Ignore it and work silently goes to the wrong agent. Rev 1 mis-classified this.

**Camp's `agent.toml` parser must tolerate unknown keys** (it already does — `pack.rs:329`) or 72/80 agents hard-fail. `camp.toml`'s `deny_unknown_fields` strictness **must never leak into `agent.toml`**. Pin with a regression test using `fallback = true`.

## 5. Agents

### 5.1 One format: the Gas City directory. The Claude Code `.md` file is retired.

```
agents/<name>/
  agent.toml            optional (agent_discovery.go:47)
  prompt.template.md    canonical; or prompt.md.tmpl; or prompt.md
  namepool.txt          optional
```

Identity is the **directory name** (`agent_discovery.go:51`), qualified by the import's binding (§7.1): the agent at `.camp/imports/gc/agents/run-operator/` is routable as `gc.run-operator`, and only as that. gc **silently ignores unknown `agent.toml` keys** — it decodes with `toml.Decode` and discards the metadata (`:48`), and the fatal-unknown-key check guards `pack.toml` only. So camp's own keys ride along and the pack stays valid in a city.

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

**Where they install — and the two traps rev 3 left open:**

- **Camp installs a pack's `skills/` into the session worktree at `<worktree>/.claude/skills/<skill>/`**, and writes `<worktree>/.claude/.gitignore` containing `*` — a self-ignoring directory, so the worker's delivery commit (`git add -A` included) can never carry camp-generated content into the operator's repo. gitignore does not affect already-tracked files, so a rig that legitimately tracks its own `.claude/` content is unharmed — **but if the rig tracks `.claude/.gitignore` itself, or tracked files collide with a skill camp would install, dispatch refuses loudly**: v1 has no merge semantics for an operator-owned `.claude/` directory.
- **The allowlist can silently disable what was just installed.** §5.2's `tools` is operator-owned; if an agent's pack ships `skills/` and the resolved allowlist lacks `Skill`, camp **refuses to spawn**, naming the two remedies: add `Skill` to `[agent_defaults].tools`, or set `skills = false` on the import — an explicit opt-out that also skips the install. Never a silent no-op.

`commands/` (Claude Code slash commands) remain out of scope and are **reported as ignored** at import.

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
| `hook --claim --json` | 154 | discovery **+ atomic claim**. `{schema_version, ok, action:"work"\|"drain", reason, bead_id, assignee, route}`. Exit 0 on work; **exit 1 on drain** unless `--drain-ack` → 0. `route` is the **qualified agent name**, resolved through the binding namespace (§7.1), and equals the bead's `gc.routed_to` byte-for-byte (§6.1). Camp's `camp claim` is only the final flip — discovery, routing, `gc.work_branch` stamping and session pointers are new. |
| `bd show/update/close/list/ready/create` | 308+ | the worker data plane. `--json`, `--set-metadata k=v`. Camp's outcome vocabulary already matches gc's (`pass`/`fail`; `shipped`/`no-op`/`blocked`/`abandoned`, `vocab.rs:47,57`) — pinned to it on purpose. |
| `runtime drain-ack` | 185 | the session-exit handshake. **A gc worker may not legally exit without it.** |
| `prime` | 31 | render the agent's prompt template to stdout |
| `mail` | 72 | v1 serves the corpus's actual usage: `send` (all 10 v1-relevant calls are `send human`) — §8.2 |
| `convoy status --json` | 9 | worker-facing read |

**Unknown subcommands and flags FAIL FAST**, naming exactly what the pack asked for and which pack asked. Never a no-op — a silently-ignored `bd update --set-metadata gc.outcome=pass` is a corrupted ledger.

**But failing fast is not enough, because the caller swallows failures.** The worker fragment opens its claim block with `set +e` and routes every failure into `sleep 2; continue` inside an **unbounded `while true` loop**. A refused `bd` call there does not surface as a fast failure — it produces a silent spin. So **every shim refusal also appends a ledger event** (`shim.refused`, naming pack / agent / verb / flag). The operator sees it even when the bash block eats it. Without that, "fail fast" is a claim the architecture cannot honour.

*(Scale note: within the 51 v1 agents, `bd mol` / `bd ready` / `bd gate` appear **only as prohibitions** in the shared fragment — "do not run broad `bd ready`". Refusing them is safe for v1. The corpus-wide counts rev 2 quoted were dominated by those negations.)*

### 6.1 The claim invariant lives on the BEAD — because the fragment trusts `bd show`, not `hook`

Every v1 agent's first act is this, from the shared `gc-role-worker` fragment:

```sh
EXPECTED_ASSIGNEE="${BEADS_ACTOR:-${GC_SESSION_NAME:-${GC_SESSION_ID:-${GC_AGENT:-}}}}"
EXPECTED_ROUTE="${GC_TEMPLATE:-${GC_AGENT:-}}"
if [ -z "$EXPECTED_ASSIGNEE" ]; then echo "CONFIG_REJECTED missing expected assignee"; gc runtime drain-ack; exit 0; fi
if ! command -v python3 >/dev/null 2>&1; then echo "CONFIG_REJECTED missing python3"; gc runtime drain-ack; exit 0; fi
```

Gas City exports those (`build_desired_state.go:1001-1003`, `bd_env.go:229-230`). Camp exports only `CAMP_DIR`, `CAMP_BEAD`, `CAMP_SESSION`, `CAMP_TRANSCRIPT` (`spawn.rs:230`). **Every one of the 51 agents would print `CONFIG_REJECTED` and exit 0** — a clean exit, for a worker that did nothing.

Rev 3 then constrained the wrong side of the equality. It pinned what `camp hook --claim --json` returns — but the fragment **overwrites** hook's values with `bd show`'s before comparing (fragment lines 127-133), and compares *those* against the environment (lines 151-158). A camp that satisfied rev 3's §6.1 exactly could still spin forever in `sleep 2; continue`.

**The invariant is therefore stated on the bead, where all three readers converge.** At claim, one ledger transaction stamps:

| where | field | value |
|---|---|---|
| bead | `assignee` | the session name |
| bead | `metadata."gc.routed_to"` | the agent's **qualified name** (`<binding>.<agent>`, §7.1) |
| bead | `metadata."gc.work_branch"` | the dispatch branch |

and the three read paths are **projections of that row, byte-for-byte**:

- `camp hook --claim --json` returns `assignee` and `route` from it;
- `bd-shim show --json` projects `assignee` and `metadata."gc.routed_to"` from it;
- the worker environment exports it: `BEADS_ACTOR` = `GC_SESSION_NAME` = `GC_SESSION_ID` = the session name; `GC_AGENT` = `GC_TEMPLATE` = the qualified name.

No independent derivations, no second formatter: one row, three projections. A mismatch anywhere is a bug in the projection, and the test that pins this class **runs the REAL fragment** (§14) — the only kind of test that would have caught rev 3's error.

**`python3` is a hard runtime dependency of the gc worker contract.** It must be declared, and added to `contrib/docker/` (which installs only `ca-certificates git tini`).

### 6.2 Session lifecycle: camp truncates gc's continuation loop, and says so

The gc worker is **not bead-scoped**. After closing a bead it loops — *"check for more routed work before draining… continue by running the same `GC_CLAIM` block again"* — and exits only via `gc runtime drain-ack`.

Camp is bead-scoped: on `bd close`, campd drops the held stdin and kills the worker after a grace (`dispatch.rs:243-269`). Two outcomes, both wrong: the worker is killed before it can `drain-ack` (violating the handshake §6 calls mandatory), or it wins the race and **claims a second bead that campd's registry does not know about** — a bead then claimed by a session campd is about to kill.

**Decision:** `camp hook --claim --json` returns **`action: "drain"`** for any session whose dispatched bead is already closed. One bead per session is enforced **at the hook**, so the worker takes its own clean `NO_ROUTED_WORK → drain-ack → exit 0` path. **`drain-ack` becomes campd's release signal** (release on drain-ack, with the existing grace timer as a backstop), not bead-close.

**Fidelity cost, stated plainly:** camp does not honour `gc.continuation_group`, and a `context = "shared"` drain is **refused**, not approximated (§9). Master §8.4 is therefore **narrowed, not upheld** — see §11.4.

### 6.3 The shims: absolute path, gitignored, dispatch-only

- **The shim must embed camp's absolute path** (`std::env::current_exe()`), not `exec camp …` by bare name. campd's PATH is a snapshot baked into the service unit at install (`service/mod.rs`, `campd_path()`), and it is not guaranteed to contain camp's own bindir — that snapshot exists so campd can find *`claude` and `git`*. A bare-name lookup would re-introduce the exact PATH bug this repo just spent five review rounds fixing.
- **`.camp/bin` is gitignored.** `gitignore::RUNTIME_DIRS` gains `bin` alongside `imports`. Autonomous workers deliver by commit from a worktree; generated executables must not ride along.
- **Attended sessions get no shims.** Camp sets the *worker child's* env, not the operator's shell. **Therefore gc pack agents are campd-dispatch-only and cannot be run attended** — stated so nobody wires `.camp/bin` into the plugin's SessionStart hook.
- The shims **shadow a real `gc`/`bd`** inside a worker for an operator who also runs a city. That is intended (the worker must talk to camp's ledger), and it is why refusals must be loud.

## 7. Import machinery and the binding namespace

### 7.1 The binding IS the namespace

**`[imports.<binding>]` in `camp.toml` declares a namespace, not just a fetch.** Every agent materialized from that import is routable as **`<binding>.<dirname>`** — and only as that. This is gc's own model, read at the layer that matters: gc stamps `BindingName` on every imported agent, and at the operator-facing scope (city- and rig-level imports) **the operator's binding always wins** — nested bindings are overridden (`pack.go:335-340` rig, same rule city-level). A qualified name in gc is *"the binding I wrote in my own config, dot, the agent's directory name."* Camp adopts exactly that.

- **Binding names are validated `[A-Za-z0-9_-]+`** (the component spec's rule, unchanged) — which means they contain no `.`, so a route splits unambiguously at the **first dot**.
- **Route resolution** (`gc.run_target`, `gc.routed_to`, `hook`'s `route`, an order's or formula's `assignee`): substitute `{{vars}}` first (46 of 99 corpus route sites are var references; every default is qualified — §2), then split at the first dot; the prefix must be a binding declared in `camp.toml`, the suffix an agent directory in that import. **A route naming an unbound binding fails fast at cook/dispatch time, naming the binding and the exact remedy** — `camp import add <source> --name <binding>`. This converts rev 3's day-one failure (every `gc.*` route resolving to nothing, silently) into a printed instruction.
- **A value with no dot** resolves as a camp-local agent (0 occurrences in the corpus).
- **Import-time visibility:** `camp import add` scans the pack's formulas' route values and reports any binding they reference that is not yet bound — the operator learns about the `gc` recipe **at import**, not at first dispatch.
- **Camp-local `<camp>/agents/` stay bare-named** — a disjoint namespace, still the operator's own layer. v1 defines **no shadowing of qualified names** (the corpus needs none); master §11's cross-pack "last-wins" is retired by amendment (§11.5).
- **Collisions are scoped by binding.** An agent name must be unique *within its binding* — the same rule `load_layer` already enforces within a layer. `gstack.review-synthesizer` and `gc.review-synthesizer` (both real: `review-synthesizer/` exists in gstack **and** in gascity/roles) coexist **by construction**. The component spec's decision 9 (cross-import collision = hard error) is **rescoped to same-binding collisions** — which, since `[imports.*]` keys are unique in TOML, can only arise from a transitive binding clash (§7.2).

### 7.2 Pack-level imports: the transitive content layer

Camp reads `[imports.<binding>]` from **`pack.toml` too** — same grammar as `camp.toml`'s (gc's shape: `source`, optional `version`). All four importing packs in the corpus declare exactly one: `[imports.gc] source = "../gascity"`.

- **Relative sources anchor at the DECLARING PACK, not the camp root.** gc resolves a pack-level relative source against the declaring pack's own directory (`resolveConfigPath`, `compose.go:1379`, reached via `pack_include.go:142` with `declDir` = the pack's dir) — and it works there because gc caches the *whole repository*, so `bmad/` and `gascity/` are filesystem siblings. Camp materializes subpath-only, so it resolves the same relation **logically**: the transitive subpath is `normalize(<declaring subpath>/<relative source>)` within the declaring pack's **own repository at its own locked commit** — for bmad at `bmad/`, `../gascity` ⇒ `gascity/`, same repo, same commit. A path that escapes the repository root is a **hard error**. (`camp.toml`-level relative sources remain camp-root-relative — matching gc, whose city-level imports resolve with `declDir = cityRoot`, `pack.go:273`. The two anchors are the same rule: *relative to the file that declared you*.)
- **Camp materializes the transitive import itself.** The operator does not (and cannot) import it by hand — it is keyed and deduplicated by **`(canonical repo, commit, subpath)`**, so bmad, gstack, compound-engineering, and superpowers all resolving `../gascity` at the same commit share **one** materialization. `packs.lock` records transitive entries with a `via = "<importing pack>"` provenance field; their location stays derived, never stored.
- **What a transitive import contributes: content layers, for its importers.** Its `formulas/` join the formula resolution layers (this is what makes `extends = ["build-base"]` and the 24 dependent formulas compile, and what supplies `[vars]` defaults like `drain_policy = "separate"`); its `template-fragments/` join fragment precedence; its `skills/`, `assets/` (the `description_file` targets), and `schemas/` likewise. All of these resolve by **bare name through layers** (§2) — no qualification needed or added.
- **What it does not contribute: agents.** `gascity` has no `agents/` directory — **importing gascity contributes zero agents in gc too** (the 12 roles live in the separate `gascity/roles` pack, §7.3). A transitive pack that *does* ship `agents/` is **refused loudly in v1**: zero exist in the corpus, and inventing semantics for them now would be a rule for nobody. gc's actual rule — the outer binding overrides the inner (`pack.go:335-340`) — is recorded here so the refusal can be lifted in v2 without a design fight.
- **Depth is 1, enforced.** A transitive pack that itself declares `[imports.*]` is refused (0 in the corpus). A transitive binding that clashes with an operator binding or another pack's transitive binding **for a different `(repo, commit, subpath)`** is a hard error naming both declarers; for the *same* key it is the dedupe case above.

### 7.3 "Nested packs" need no machinery — the corpus deploys them by explicit import

Rev 3's C4 read `gascity/roles/pack.toml` as a composition feature camp lacked. **gc lacks it too.** `DiscoverPackAgents` scans `<pack>/agents/` and nothing else (`agent_discovery.go:24`); `loadPack` composes only what `pack.toml` *declares* (`includes`, `[imports.*]`) — and gascity's declares neither. The nested pack is not auto-discovered by anyone.

What actually happens is in the corpus's own READMEs (bmad step 2, gascity/README): the operator **imports `gascity/roles` explicitly, rig-scoped, bound as `gc`**:

```toml
[rigs.imports.gc]
source = "https://github.com/gastownhall/gascity-packs.git//gascity/roles"
```

That import — not nested-pack magic — is where `gc.run-operator`, `gc.implementation-worker`, `gc.publisher`, and every other `gc.*` route comes from in a real city. Camp's v1 recipe (§3) is the same two commands with camp as the consumer. The roles pack is **self-contained**: it ships its own copy of the `gc-role-worker` fragment (byte-identical to gascity's — verified), so importing it alone renders its 12 agents.

One obligation remains: when a materialized subtree *contains* a nested `pack.toml` camp did not compose (materializing `gascity` brings `roles/` along inside the subpath), `camp import add` **reports it**: `nested pack at roles/ ("gc-roles") — not composed; import it explicitly to use it`. Visible, never silent.

### 7.4 Retained from the component spec, and what changes there

Retained: `[imports.<name>]` in `camp.toml`; a tracked `packs.lock` (`schema = 1`, keyed by the **verbatim source string**, `{version, commit, fetched}` — gc's shape, now plus `via` for transitive entries); materialization into `.camp/imports/<name>/` (gitignored); gc's source grammar verbatim; `camp import add | install | list | remove | upgrade | check`.

`pack.toml` **required** with `[pack].name` + `[pack].schema` (≤2) — gc's rule (`pack.go:2372-2391`). **`version` is not required** (gastown ships without it; gc's generated JSON schema wrongly says otherwise).

**Symlinks are dereferenced on materialization** — `packs/starter/formulas/guarded-change.toml` symlinks *outside* the pack subpath; materializing the subpath alone leaves a dangling link.

The component spec is **amended in the same wave** — it currently contradicts this section on four points (recorded in its own header): decision 9 rescoped to same-binding collisions (§7.1); `skills/` is installed, not ignored (§5.3); pack orders are an `orders/` **directory** of files (gc's v2 layout — `orders.ScanRoots` on `<pack>/orders`), not an `orders.toml`; and pack-level `[imports.*]` (§7.2) exists at all.

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

### 8.2 Comms: nudge first — and v1 mail is exactly what the corpus uses: `send human`

The packs' own doctrine: *"Default to nudge for all routine communication… If the recipient dies and restarts, do they need this message? If yes → mail. If no → nudge."*

Camp **already has nudge** — a turn into the live worker's held stdin, or `claude --resume` once it has exited. The gap is a nudge to a session that **does not exist yet** (queue it, materialize on spawn).

**v1 mail is operator-directed only, because that is all the v1 corpus contains.** Every one of gascity's 10 mail references is `gc mail send human` (§2) — a worker escalating to the operator from a workflow step. **v1 has no agent-to-agent mail**, and rev 3 designed one anyway, complete with a per-turn `mail check --inject` hook and an amendment to invariant 1 to license it. Both are **withdrawn** (§11.2):

- **`camp mail send human <subject> …`** creates a mail bead (`type = "mail"`, the type camp already has: `fold.rs:13`, dispatch-excluded, `readiness.rs:84-88`), with `from` = the sending session and gc's flag spelling. `send` to any other recipient is **refused, naming gastown/v2** — not silently accepted into a mailbox nobody reads.
- **The operator reads it**: `camp mail inbox | read | archive | count`, and `camp mail check` with gc's exit-code contract (**0 = has mail, 1 = empty**; a status line depends on it). The statusline badge and `/status` surface unread count — the operator-side pull is a human reading their own mailbox, not a poll.
- **No injection hook, no per-turn check, no worker-side delivery.** Those exist to deliver mail *to agents*, and no v1 agent receives mail. They move to v2 (gastown), where agent-to-agent mail is real — **together with the invariant-1 amendment they require.** v1 leaves invariant 1 intact: campd never polls, and workers never poll.
- **Sanitize sender/subject/body against a `</system-reminder>` breakout anyway** — mail is untrusted text that will be rendered into an operator's session by the statusline/`/status` path, and v2 will inject it into agent context. Gas City learned this the hard way.

*(The `gc mail send mayor/` trailing-slash trap and recipient mailboxes ride to v2 with gastown — that is where non-human recipients exist.)*

### 8.3 Standing sessions are v2, with gastown

`[[named_session]] mode = "always"` exists so *there is somebody to receive mail*. Camp's worker dies with its bead, so today **a camp mailbox has no recipient**. That is the whole of the gastown gap, and `min_active_sessions > 0` is a standing process with no bead — **a direct violation of invariant 2**.

v2 designs it; v1 does not need it (0 named sessions across 51 agents; every v1 mail call addresses the human).

## 9. Formulas

**The ladder is a TEST, not a claim.** Three independent attempts to compute it produced three different answers (48/75/87/100, then 34/61/87/97, then 32/59/85/98) — the discrepancies were all in *which rung owns which key*, most damningly step `metadata`, which is routing and not annotation. A number nobody can reproduce is a boast. So:

**The compatibility gate (§10) asserts the exact count at each phase, per key set.** The spec commits to the *shape*, not a headline:

| phase | key set added | corpus loading |
|---|---|---|
| 2a | dead keys ignored; annotations; `contract`; `description_file` (**53** formulas); step `metadata` (incl. `gc.run_target` routing via §7.1) | pinned by test |
| 2b | `vars`, `condition` (**13**) | pinned by test |
| 2c | `extends` (48) | pinned by test |
| 2d | `type`, `template`, `expand`, `expand_vars`, `children` | pinned by test |
| **2e** | **`drain` (13)** — moved here from phase 5. **v1's three packs cannot run without it**: their headline formula's `implement` step *is* a drain step. | pinned by test |

**Permanently refused, and therefore the ceiling is below 100:** `phase = "vapor"` (2 formulas — v1 materialization semantics) and `scope-check` / `gc.scope_*` (1). Rev 1 claimed 100/100 while simultaneously refusing these. **The ceiling is 97–98, and the gate will say exactly which.**

Semantics an implementer must get right (each verified in gc's compiler):

- **`description_file` (53 formulas)** — the file's *contents replace the step description* at parse time, and those steps typically carry **no inline description**. Ignore it and the worker gets **zero instructions**. `../assets/…` resolves **through the formula layers** (highest wins — that's how a pack shadows prose while inheriting structure), which with §7.2 means a child pack's formula reaches the gascity pack's assets. >4096 bytes ⇒ a pointer prompt, so the *path* must still resolve.
- **`extends` (48)** — child seeds scalars; parents' steps **append**; a child step whose `id` matches a parent's **replaces it whole, in place, preserving position**. No field-level merge. `advice`/`pointcuts` are dropped entirely. Parents resolve by bare name through the formula layers — §7.2 is what puts `build-base` in them.
- **`condition` (13)** — `==` and `!=` only; LHS must be a single `{{var}}`. **False ⇒ the step is PRUNED with its children**, and dangling `needs` edges are silently dropped.
- **`{{var}}` substitution** — applies to `title`, `description`, `assignee`, metadata values, `notes`, tags. **Not** to `id`, `needs`, `check.path`, or `drain.formula`. An undefined var **keeps the literal placeholder**; the residual check is enforced **only on `title`**. Reproduce that asymmetry or diverge.
- **`type = "expansion"`** — the formula is **not directly runnable**; it supplies `template` steps for `expand`.
- **`drain` (13, phase 2e)** — runtime fan-out, with the semantics pinned to gc's compiler defaulting (`compile.go:579-608`):
  - **`context = "shared"` is REFUSED, loudly** — camp truncates gc's continuation loop (§6.2), so a shared-context drain cannot share a worker session and camp will not pretend it does. The refusal names the formula, the step, and the `drain_policy = same-session` var that selects it. **The default path runs**: all three v1 build formulas gate their two drain steps on mutually exclusive `condition`s over `{{drain_policy}}`, whose default is **`"separate"`** (declared in the gascity parent `build-base.formula.toml:61-63` — one more thing §7.2 is load-bearing for).
  - **`on_item_failure`** — `skip_remaining | continue`; gc's default is `continue` for separate context (`skip_remaining` applies to the shared context camp refuses). `continue` = an item's failure does not stop the remaining items; the drain's own outcome reflects the failures at finalize.
  - **`item.single_lane`** — items dispatch one at a time, never in parallel. (gc *requires* it for shared drains; for separate drains it is an authored throttle.) Camp honours it mechanically: the drain's ready items enter dispatch with concurrency 1.
  - **`member_access = "exclusive"` (25 uses)** — a per-member reservation, stored where gc stores it: **as metadata on the member bead** (`gc.exclusive_drain_reservation`, `beadmeta/keys.go:93`), written in the reserving transaction, released at drain end. A second drain reserving a held member **fails the reserving drain loudly** — never two drains mutating one bead. Camp mirrors the key verbatim (invariant 7).
- **21 formulas declare no `contract` at all** — they are not `graph.v2` in gc. Camp must **not** run them under graph.v2 semantics by default. Refuse, or state the fidelity risk explicitly; do not silently assume.

## 10. The compatibility gate

Today's CI proves **camp ⊆ gc** (camp's corpus compiles under the real gc compiler). That is **the wrong direction** and proves nothing about running gc packs.

The new gate: **fetch the real `gascity-packs` corpus at a pinned ref and assert camp loads and compiles exactly what it claims — and refuses, by name, what it does not.** The claimed numbers become tests, seeded by `measure_corpus.py`. A regression from 85 to 60 fails CI.

**The pin is a commit sha of the corpus repo: `ci/gc-compat/GCPACKS_REF`** — the exact mold of `GASCITY_REF`, moved deliberately by PR. Rev 3 planned to pin via `registry.toml`'s per-pack manifest hashes instead, and that cannot work: **the registry registers 11 packs, and `bmad`, `gstack`, `compound-engineering`, and `superpowers` are not among them** (verified: gascity, gastown, cass, discord, github, slack-full, slack-channel, slack-mini, pr-pipeline, contributing, oversight-rig). A hash apparatus that cannot name 3 of the 4 v1 packs pins the wrong thing. A git commit sha is already a content pin for the whole tree — the v1 packs, the transitive gascity subpath, and the fragment the §14 spin test executes, all at once.

*(The registry's bespoke manifest hash — `validate_registry.py:94-136`: per-file `"<relpath> <perm> <sha256>"` lines, symlinks as mode `120000`, hashed pre-dereference — remains documented here for the future registry-install verb, which is out of scope (§15). It is not part of the CI gate.)*

**Do not vendor the corpus.** `gastownhall/gascity-packs` has **no top-level LICENSE**; it is a mixed-license tree (third-party vendored dirs carry their own), i.e. all-rights-reserved by default, and gascamp is AGPL-3.0. Fetch at `GCPACKS_REF`; never commit the fetched tree.

Both gates run — camp ⊆ gc (invariant 6) and gc-corpus-on-camp (this one). Without the new one, "compatible" is a claim, not a fact.

## 11. Decision-record amendments (this section IS the spec PR)

AGENTS.md forbids re-litigating the record without one. Five changes (rev 3's invariant-1 amendment is withdrawn, not carried):

1. **Master §11 — "a role is a Claude Code agent file, zero invented formats."** *Retired.* Camp's native agent is a Gas City directory (§5.1). The trade is deliberate: gc compatibility is worth more than bare-Claude-Code portability, because packs+formulas *are* the process. (The companion clause — *machinery ships no roles* — **survives**: camp's binary carries only a pack *source URL*. Note gc's own machinery ships one agent, `core/control-dispatcher`, `prompt_mode="none"` — so the narrow reading is what holds in gc too.)
2. **Invariant 1 — "no polling loops, anywhere." *Upheld — rev 3's amendment is WITHDRAWN.*** Rev 3 narrowed the invariant to license a per-turn `mail check --inject` hook, and bought nothing with it: every v1 mail call is `send human` — there is no agent recipient to inject into (§8.2). The hook, and the amendment licensing it, move to the gastown phase (v2), which must re-open this invariant explicitly if it still needs to. v1 ships with invariant 1 intact.
3. **Invariant 5 — "no fallbacks."** *Upheld, and made sharper:* a pack agent with no resolvable tool allowlist is an **error**, not a default (§5.2). Camp never inherits gc's `unrestricted`.
4. **Master §8.4 — workers spawn per bead and exit on close.** *Narrowed, not upheld* (rev 2 claimed "upheld" and was wrong). The gc worker contract camp adopts mandates a **multi-bead continuation loop** and a **`drain-ack` exit handshake**; camp is bead-scoped and kills the worker on close. Camp therefore **truncates gc's session protocol**: `hook --claim` returns `drain` once the session's bead is closed, and `drain-ack` becomes the release signal (§6.2). **Fidelity cost:** `gc.continuation_group` is not honoured, and `context = "shared"` drains are refused (§9). Standing named sessions (which gastown requires) remain **v2**, and that phase must re-litigate **invariant 2** explicitly (§8.3).
5. **Master §11 — "resolution is last-wins with local definitions highest."** *Amended:* imported agents live in **binding-scoped namespaces** (§7.1), so cross-pack shadowing is structurally impossible and "last-wins" between packs no longer describes anything. What survives: camp-local `<camp>/agents/` and `<camp>/formulas/` remain the operator's own layer (bare names; formulas still shadow imported formulas by name, highest-wins). v1 defines no shadowing of *qualified* names. The component spec's decision 9 is rescoped to match (§7.1).
6. **Master §11's `skills/` corollary.** Retiring "a role is a Claude Code agent file" (amendment 1) does not license dropping a pack's `skills/`: **13 of the 51 v1 agents' prompts depend on it** (§5.3). Camp installs pack skills into the session worktree's `.claude/skills/`, self-gitignored. A format decision must not silently become a content decision.

## 12. Phases

1. **Import machinery + the binding namespace + pack loader.** Fetch/lock/install, git hardening, `trust_exec`. `pack.toml`, **pack-level `[imports.*]` with transitive materialization** (§7.2), **binding-qualified agent resolution** (§7.1), agent directories, `formulas/`, `orders/` **directories**, `skills/` install (§5.3). Fixes #80; retires the `.md` agent format; fixes #85 by construction.
2. **Formulas** (§9), phase-gated by key set, each rung pinned by the gate — **including `drain` (2e)**, without which none of v1's packs run.
3. **The worker contract** — `gc`/`bd` shims (§6.3), the **bead-side claim invariant and worker environment** (§6.1), `hook --claim --json` with qualified routes, `runtime drain-ack` as the release signal (§6.2), `python3` in the container, **and #86 (`--verbose`)**. A gc worker closes a gc bead end-to-end — proven by the real-fragment test (§14).
   **v1 target: `bmad | gstack | compound-engineering` + transitive `gascity` + `gascity/roles` bound as `gc`** (§3).
4. **Mail** (§8.2) + `prime`. Operator-directed `send human` + inbox — the corpus's actual v1 surface; no injection hook.
5. **The control plane** — see `2026-07-12-camp-control-plane-design.md`, which has its own phase 0 (the read channel).
6. **gastown** — standing sessions, pools, 66 mail calls, agent-to-agent mail (and the invariant-1 re-litigation it requires). The phase that re-opens invariant 2.

## 13. Security

**Fetch.** Camp's first production git subprocess. gc's untrusted-remote hardening ported verbatim (`internal/git/git.go:385-395`): `http.followRedirects=false`, `protocol.allow=never` + an explicit `https/http/ssh/git/file` allowlist (this is what blocks `ext::` — arbitrary command execution), `core.hooksPath=/dev/null`, `core.fsmonitor=false`, `core.untrackedCache=false`, sanitized `GIT_*` env. One helper owns the flags; a test asserts the argv byte-for-byte. The threat model is **not** "the operator typed the URL" — `camp.toml` and `packs.lock` are **tracked**, so a source arrives via `git pull`, a PR branch, or CI. **Transitive imports (§7.2) widen this surface**: a pack the operator vetted can declare a source the operator never saw. Mitigations: transitive sources are constrained to the declaring pack's own repository (the relative-path rule — a remote transitive source is refused in v1, 0 in corpus), they appear in `packs.lock` with `via` provenance, and `camp import add` prints them.

**Execute — `trust_exec`, default deny.** `[imports.<name>] trust_exec = true` gates a formula's `check.path`, `pre_start`, `exec` orders, and `condition` shell. `camp import add` **inventories the executable content** — including what arrived transitively — and prints the command to enable it. `trust_exec` on an import covers its transitive materializations: the operator trusts the pack *as deployed*, and the pack's formulas execute its parent's `check.path` scripts as their own.

**Tools.** §5.2. No agent runs without a resolved allowlist. §5.3's `Skill` interaction refuses rather than silently disabling.

**The money invariant.** An order fires a formula; a formula dispatches workers; workers cost real money.

> **Nothing an import brings may fire until the operator names it in `[orders] enabled`.**

Imported orders load, validate, and appear in `camp order ls` as **disabled**, with their source.

## 14. Testing

- **No network.** Git-backed imports run against local `file://` repos in a temp dir — the real clone/lock/materialize path, including a fixture repo with a `bmad`-shaped pack declaring `[imports.gc] source = "../gascity"`, so transitive resolution, dedupe, and the repo-escape error are all exercised.
- **No API spend.** Workers are `#!/bin/sh` fakes. Never a real `claude`.
- **THE REAL-FRAGMENT TEST — the one that catches the §6.1 class.** Render the actual `gc-role-worker` fragment from the corpus at `GCPACKS_REF` and execute it under `sh` with the real shims against a fixture camp (fake `claude`, real ledger). Assert the worker **claims, closes, drain-acks, and exits**. Give it a deadline: the fragment's failure mode is an unbounded `sleep 2; continue` loop, so a hang IS the failing signal. Three prior revisions specified this contract wrong in three different ways; a test that runs the genuine consumer is the only fix that stays fixed. *(Also assert the projection property directly: `hook --claim --json`'s `{assignee, route}`, `bd-shim show --json`'s `{assignee, metadata."gc.routed_to"}`, and the exported env agree byte-for-byte.)*
- **Routing tests:** a formula routing to `gc.run-operator` with the `gc` binding absent must fail at cook time, naming the binding and the `camp import add … --name gc` remedy — never dispatch to nothing. With the binding present, the bead's `gc.routed_to` carries the qualified name.
- **Collision tests:** `gstack.review-synthesizer` + `gc.review-synthesizer` coexist (the day-one crash rev 3 would have shipped); two agents with one name **within** a binding still hard-error.
- **Drain tests:** `drain_policy = same-session` is refused naming the step; an exclusive reservation held by one drain fails a second drain's reserve; `single_lane` items never run concurrently.
- **The money invariant gets a test that can fail:** an imported pack with a due cron order fires **nothing** until `[orders] enabled` names it.
- **`trust_exec` likewise:** a pack formula carrying a `check.path` executes nothing untrusted — including a `check.path` reached through a transitive parent formula.
- **The tool-allowlist refusal likewise:** an agent with no resolvable `tools` must not spawn; an agent whose pack ships `skills/` with `Skill` missing from the allowlist must not spawn.
- **The skills gitignore:** after a dispatch with skills installed, `git -C <worktree> status --porcelain` shows nothing under `.claude/`.
- **`fallback = true` must parse and be ignored** (72/80 real agents depend on it).
- The git hardening argv is asserted byte-for-byte. A dropped flag is a removed fence.
- Every new test must die against a mutation of the code it guards.

## 15. Out of scope (#84)

`[[exports]]`, semver constraint solving, the pack registry and its manifest-hash verification (§10), a shared machine-local cache, credentials for private pack repos, `why`/`--tree`/`prune`/`status`/`migrate`, gc's `overlay/` mutation semantics, `commands/`, `doctor/` checks, ACP, provider presets, tmux, and a remote API / Web UI / per-agent VM isolation (the control-plane spec constrains the design so these stay possible; it does not build them).

**No longer out of scope:**
- **The transitive import graph** (single-level, §7.2) — rev 2 listed it here while its own goal statement required it.
- **The binding namespace** (§7.1) — rev 3 listed "namespaces" here while its own §6.1 demanded qualified names. It is load-bearing for every route in the corpus; only `[[exports]]` (re-exporting a nested binding under a new name) stays out, because nothing in the corpus uses it.

## 16. Corrections to earlier revisions' numbers

Re-derived via `measure_corpus.py` and confirmed by review. Wrong numbers, and where the right ones now live:

| claim | earlier rev | actual |
|---|---|---|
| `description_file` | 67 formulas (rev 2; rev 3 §9 prose still said it) | **53** |
| `condition` | 17 (likewise) | **13** |
| `max_active_sessions` | 2 | **7** |
| gastown mail calls | 46 | **66** |
| literal `gc hook` refs | 140 | **151** (the load-bearing half — `{{cmd}} hook` = **0** — was right) |
| `bd mol` / `bd ready` | "102 / 80 refs, no camp equivalent" | corpus-wide counts **dominated by prohibitions**; **0 real invocations** among the 51 v1 agents |
| route counts | review notes quoted `gc.run-operator` = 82 | **not reproducible.** `measure_corpus.py`: 55 literal + 46 as `{{implementation_target}}`, whose per-formula defaults are all qualified (`bmad.story-implementer` 8, `superpowers.implementer` 16, …). The load-bearing fact survives and is now measured: **0 bare route values, corpus-wide** |
| gascity mail | "10 mail calls" (uncharacterized) | 10 refs, **all `gc mail send human`** — 6 in workflow assets, 4 in gc's own tests. No agent recipient exists in v1 (§8.2) |

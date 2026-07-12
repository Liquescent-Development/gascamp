# Known defects in the current specs — read before touching them

**Date:** 2026-07-12
**Applies to:** `2026-07-12-gas-city-pack-compatibility-design.md` (rev 3) and `2026-07-12-camp-control-plane-design.md` (rev 2)
**Status:** ADDRESSED — compat rev 4, control-plane rev 3, and component-spec rev 3 fix every finding below (resolution map at the end of this file). This file remains as the record of the findings and their evidence; the findings' text is unchanged and still describes rev 3 / rev 2.

Three revisions of the compat spec each drew four Criticals from adversarial review. **Every one was the same error: asserting camp has a mechanism, and stopping one `grep` short of where the corpus actually lands.** The direction is sound and the measurements are now solid; the mechanisms are not. This file exists so the next pass starts from the findings instead of rediscovering them.

## How to re-derive every number

`ci/gc-compat/measure_corpus.py` — the script that produces the spec's numbers. **Run it before trusting any figure in either spec.** It also seeds the compatibility gate the compat spec's §10 calls for.

Corpora are **not** in the repo (licensing: `gascity-packs` has no top-level LICENSE). Fetch:

```sh
git clone --filter=blob:none --no-checkout https://github.com/gastownhall/gascity /tmp/gascity   # gc source
git clone --depth 1 https://github.com/gastownhall/gascity-packs /tmp/gcpacks                   # the corpus
npm pack @anthropic-ai/claude-agent-sdk    # the CLI control protocol's ground truth (sdk.mjs)
```

**Two traps in measuring this corpus, both of which produced wrong numbers in earlier revisions:**
1. **Never regex TOML.** Real formulas have multi-line `description = """…"""` blocks; a regex matches keys *inside the prose*. Use `tomllib`.
2. **Glob `formulas/*.toml`, not `*.formula.toml`.** The 8 `gastown/formulas/mol-*.toml` break the naming convention; the narrow glob yields 92 formulas and makes every downstream number wrong.

---

## Gas City pack compatibility — 4 Criticals

### C1. Camp has no binding namespace. This is the root cause of C3b, C4, and M9.

**Every route in the corpus is `<binding>.<agent>`:**

```
gc.run-operator                                82
gc.review-synthesizer                          11
compound-engineering.ce-code-review-selector   11
gstack.review-synthesizer                       8
gc.publisher                                    8
```

gc stamps the import binding onto every agent — `internal/config/pack.go:339`, `agents[i].BindingName = bindingName`. An agent's routable identity is `<binding>.<dirname>`; the directory is bare (`bmad/agents/architect`), the route is not (`bmad.architect`).

**Camp resolves agents by bare name across a flat layer stack**, and the compat spec's §15 puts namespaces **out of scope** while §6.1 demands "the agent's **qualified name**" — the word used without a scheme. The two sections contradict each other.

**Consequences:** camp cannot resolve `bmad.architect`; and `GC_TEMPLATE` can never equal `gc.routed_to`, so the worker fragment hits its `sleep 2; continue` and **spins forever** (see C2).

**Fix:** make the import binding first-class. Read `[imports.<binding>]` from **both** `camp.toml` and each `pack.toml`; stamp the binding on every agent; resolve `gc.run_target` through it; export the qualified name. **Delete "namespaces" from §15 — it is load-bearing for routing, not optional.**

### C2. §6.1 pins the wrong side of the equality — the fragment compares against `bd show`, not `hook`.

The shared worker fragment lets `bd show` **overwrite** hook's values *before* the comparison:

```sh
127  SHOW_ASSIGNEE="$(printf '%s' "$SHOW_JSON" | json_pick assignee)"
129  if [ -n "$SHOW_ASSIGNEE" ]; then CLAIM_ASSIGNEE="$SHOW_ASSIGNEE"; fi   # bd show WINS
131  SHOW_ROUTE="$(... | json_pick metadata:gc.routed_to)"
133  if [ -n "$SHOW_ROUTE" ]; then CLAIM_ROUTE="$SHOW_ROUTE"; fi            # bd show WINS
151  if [ "$CLAIM_ASSIGNEE" != "$EXPECTED_ASSIGNEE" ]; then sleep 2; continue; fi
```

Rev 3 constrains only `camp hook --claim --json` — **the side the fragment throws away.** Camp can satisfy §6.1 exactly and still spin forever.

**Fix:** state the invariant on the **bead**: at claim, camp stamps `assignee = <session name>` and `metadata."gc.routed_to" = <qualified agent name>`, and `bd-shim show --json` projects exactly those bytes. **Pin it with a test that runs the REAL fragment against a fake worker and asserts it exits rather than loops.** That test is the only thing that catches this class.

### C3. `../gascity` resolution is asserted, never specified — and the collision rule crashes the v1 command.

**(a)** §3 says "a relative `../gascity` source must resolve after materialization (**see §7**)". **§7 says nothing about it.** And the component spec — the named detail spec — says the *opposite* of gc: it resolves relative sources against the **camp root**, yielding `<camp>/../gascity`. gc resolves against `declDir`, **the declaring pack's own directory** (`pack.go:980`), and caches the *whole repo*, so `bmad/` and `gascity/` are siblings and `../gascity` resolves for free. **Camp materializes subpath-only, so the sibling does not exist.**

**(b) The collision bomb.** `review-synthesizer` is defined by **both `gstack` and `gascity`**. The component spec's decision 9 makes a cross-import name collision a **hard error** — so `camp import add gstack` + gascity **crashes on day one, on the spec's own named v1 target.** In gc they are `gstack.review-synthesizer` and `gc.review-synthesizer` — distinct qualified names. This is C1 cashing out as a crash.

**Fix:** (i) read pack-level `[imports.*]`; (ii) anchor relative sources at the **declaring pack's** materialized dir; (iii) **camp materializes the transitive import itself** (the operator does not import it separately); (iv) dedupe by `(source, commit, subpath)`; (v) scope names by binding, so decision 9 only fires on a true same-binding collision.

### C4. `gascity` has no `agents/` directory. The v1 transitive dependency contributes ZERO agents.

```
gascity/            assets  formulas  roles  schemas  skills  template-fragments  tests
gascity/agents/     DOES NOT EXIST
gascity/roles/pack.toml   →  [pack] name = "gc-roles"      ← a NESTED PACK
```

The 12 roles live in `gascity/roles/agents/`. Camp layers `<pack>/agents` and only that — so **every `gc.*` route (82 × `gc.run-operator`, plus `gc.implementation-worker`, `gc.publisher` …) resolves to nothing, in all four v1 packs.** gc composes nested packs and overrides the inner binding with the outer (`pack.go:335-337`). Neither camp spec has any concept of a nested pack.

**Fix:** specify nested-pack discovery (walk for a nested `pack.toml`, stamp the *outer* binding), or import `gascity/roles` explicitly and say how the `gc.` binding attaches.

### Also (High/Medium)

- **The compatibility gate cannot pin 3 of the 4 v1 packs.** `registry.toml` registers 11 packs; **`bmad`, `gstack`, `compound-engineering` and `superpowers` are not among them.** §10's whole hash-verification apparatus applies to packs v1 does not run. Pin the v1 packs by commit sha directly and say so.
- **Invariant 1 was amended for nothing.** All **10** of gascity's mail calls are `mail send **human**` — a first-class gc mailbox (`cmd_mail.go:863`). **v1 has no agent-to-agent mail**, so the per-turn `mail check --inject` hook is not needed, and §11.2's amendment of *"no polling loops, anywhere"* buys nothing. v1 mail = `send` (to human) + operator-side `inbox`. Defer the hook **and the amendment** to gastown (v2), where mail genuinely is agent-to-agent.
- **`context = "shared"` drains are silently approximated**, which §3 and §4 both forbid. Mitigating: the two drain steps are mutually exclusive on `{{drain_policy}}`, which **defaults to `separate`** — so camp *can* run the default v1 path. But an operator setting `drain_policy = same-session` gets a silently degraded run. **Refuse it.** Also unspecified: `on_item_failure`, `item.single_lane`, and **where camp stores `member_access = "exclusive"` reservations** (25 uses).
- **Master §11's "last-wins" layering law is overturned with no amendment** (the component spec's decision 9). Binding-scoped names (C1) dissolve the need for it entirely.
- **`skills/` install path unspecified**, and it collides with the worktree-commit model: `<worktree>/.claude/skills/` gets **committed into the operator's repo**. Also, §5.2's operator-owned `tools` allowlist can **silently disable** the skills §5.3 installs, if `Skill` is not in the list.
- **Stale prose contradicting rev 3's own corrections:** §9's prose still says `description_file` (67), `condition` (17), and **`drain … phase 5`** — the exact thing rev 3 moved to phase 2.
- **The component spec is stale beyond its declared overrides** — it still says `skills/` is "IGNORED by camp, a design decision, not an oversight", still says `orders.toml` (a file) where the umbrella says `orders/` (a directory), and contains **no pack-level `[imports.*]` machinery at all** — the very thing §7 delegates to it.

---

## Control plane — 3 blockers

### B1. `notify` is a lossy heartbeat, and the control channel has no safety net.

Rev 2 justified the file-tail with *"not new machinery — patrol already does exactly this."* **False. Patrol WATCHES; it never TAILS.** It reads no content, keeps no offset (`patrol.rs:168-194` sets a touched-flag and writes one self-pipe byte; `drain_touched` resets a timer). It tolerates dropped events **only because an armed stall timer catches them** (`patrol.rs:513-522`: *"a false stall costs one nudge"*).

`notify` is documented-lossy. On inotify overflow it emits `EventKind::Other` + `Flag::Rescan` **with an empty `paths` vec** — and camp's handler iterates `event.paths`, so **no self-pipe byte, no wake, the Rescan is discarded.**

The control channel has no net: §5.3 deliberately removes every timeout, and invariant 1 forbids a poll. **One dropped event = a `can_use_tool` campd never reads = a worker blocked forever that campd does not know is blocked.**

**Fix:** delete the "not new machinery" claim (offsets, partial-line buffering, reopen-after-restart, delivery reliability are all absent from patrol and *are* the hard part). Handle `Flag::Rescan`/empty-path events by re-reading every tailed file to EOF. Demote the watch to a **latency optimization**: re-read tailed files to EOF on every campd wake, so correctness never depends on a delivered event.

### B2. Patrol's stall ladder SIGKILLs a BLOCKED worker. "It does not time out" is false.

A worker parked on `can_use_tool` writes nothing and emits no events. Its stall timer (default 10m) fires → `agent.stalled{nudge}` → the nudge cannot unblock a CLI parked on a promise → the ladder escalates → `LadderAction::Restart` → `kill_worker` → **SIGKILL.**

So §5.3's central promise — *"it does not time out into a default, and it does not proceed"* — is false. **It times out into a kill**, and rev 2 never mentions it. §5.3.2 worried about the `max_workers` slot and missed the ladder, which is far worse: it destroys the work.

**Fix:** state what BLOCKED does to the stall timer. Either **disarm** it on `permission.pending` and re-arm on the decision (a blocked worker is not stalled — it is correctly waiting), or exempt BLOCKED from the ladder (the annotate-only mold). Pick one **in the spec**.

### B3. Bounding and tailing are mutually exclusive. The spec mandates both.

Worker stdout is `File::create` → `O_WRONLY|O_CREAT|O_TRUNC`, **no `O_APPEND`** (`spawn.rs:265`). The child owns the offset for its whole life; campd is not the writer.

- **Rotate:** the worker keeps writing to the renamed **inode**. campd's fresh file stays empty — every later `can_use_tool` is lost, silently and permanently.
- **Truncate:** without `O_APPEND` the offset does not reset. The next write lands at the old offset → a **sparse hole of NUL bytes** → the line reader gets NULs, not JSON.

So §9's *"a byte cap with rotation"* is **not implementable against a live worker.**

**Fix:** the live stream file is **append-only, never rotated or truncated under a live writer**. Make `session.subscribe` cursors **byte offsets** — which makes them durable across a campd restart for free. Bound at session end (reap-time dispose/compress). For a live bound, a per-session byte ceiling that on breach **fails the session loudly** (invariant 5) rather than corrupting the channel.

### Also

- **The invariant-1 evidence is false.** `idle_campd_cpu_delta_zero_and_rss_under_20mb` (`perf_daemon.rs:232-251`) scaffolds an **empty camp** — no bead, no session, so **zero patrol watches** and, since connections are deregistered after each response, **zero clients**. The spec cites this gate for both "N tailed stdout files" and "N idle subscribers"; it tests neither. *(What it does prove: the camp.toml config watcher is a live `notify` watcher for the whole idle window, so "a notify watcher costs 0.0% CPU" is genuinely demonstrated.)* **Extend the gate to hold M quiescent workers with tailed stdout files and N connected subscribers.**
- **§5.3.3's kill-on-adoption can kill a healthy worker** (if campd wrote the `control_response` and died before appending the decision event) **and strands the bead** — the dispatchable set excludes ever-sessioned beads, and only a `"patrol restart"`-reasoned crash re-hooks one. Name the crash reason and say whether the bead re-hooks.
- **§5.3.1's fail-fast would refuse every agent camp ships today.** F7's pinned config is `bypassPermissions` + explicit `--allowedTools`. If `--permission-prompt-tool stdio` is added unconditionally, **every existing camp stops dispatching.** The flag must be per-agent.
- **The real-`claude` gate is mostly FREE.** Argv rejection happens at CLI validation *before any turn* (that is #86's signature); `initialize` and an `interrupt` before any turn are CLI-local — **$0**. Only the forced `can_use_tool` leg needs a paid turn, and `make e2e` already exists as the sanctioned envelope. **Split the gate into a $0 tier and a paid tier** rather than making a paid run a release blocker by side effect.
- **`subscribe` needs numbers, not adjectives:** an outbound-buffer cap (the `MAX_REQUEST_BYTES` mold — it is otherwise campd's only unbounded memory), and a bounded server hello so a `camp watch` against a **wedged** campd is distinguishable from a quiet one (issue #55's bug class).

---

## Resolution map (compat rev 4 · control-plane rev 3 · component rev 3)

| finding | resolved by |
|---|---|
| C1 binding namespace | compat §7.1 (first-class binding; routes split at the first dot; unbound binding = named fail-fast); §15 un-scopes it. One correction to this file's evidence: the route table above (`gc.run-operator` 82, `compound-engineering.ce-code-review-selector` 11) is **not reproducible** — `measure_corpus.py` (now deriving `[vars]`-resolved routes) measures 55 raw + 46 via `{{implementation_target}}` defaults. The load-bearing fact survives strengthened: **0 bare route values, corpus-wide, measured.** |
| C2 wrong side of the equality | compat §6.1: the invariant moved to the bead row; hook/`bd-shim show`/env are three byte-projections of it. §14: the REAL-fragment test with a deadline (a hang is the failing signal). |
| C3a `../gascity` resolution | compat §7.2: pack-level `[imports.*]`, relative sources anchored at the declaring pack's subpath in its own (repo, commit); escape = hard error; camp materializes transitively, deduped by `(repo, commit, subpath)` with `via` in the lock. |
| C3b collision bomb | compat §7.1 + component decision 9 rescoped: `gstack.review-synthesizer` / `gc.review-synthesizer` coexist by construction; §14 pins it. |
| C4 gascity has no agents/ | compat §7.3: gc doesn't auto-discover `gascity/roles` either — the corpus READMEs deploy it as an explicit rig-scoped import bound `gc`. Camp's v1 recipe (§3) mirrors it; nested `pack.toml` is reported, never composed silently. No discovery machinery invented. |
| gate can't pin 3 of 4 packs | compat §10: pin = `GCPACKS_REF` commit sha (the `GASCITY_REF` mold); the registry manifest-hash moves out of the gate, documented for the future registry verb. |
| invariant-1 amendment for nothing | compat §8.2 + §11.2: v1 mail = `send human` + operator inbox; the inject hook AND the amendment are withdrawn to v2/gastown. Invariant 1 ships intact. |
| shared drains silently approximated | compat §9 drain bullet: `same-session` refused loudly; `on_item_failure`/`single_lane` semantics pinned to gc's compiler defaulting; exclusive reservations stored as member-bead metadata (`gc.exclusive_drain_reservation`, gc's key verbatim). |
| layering law overturned unamended | compat §11.5: master §11's cross-pack last-wins formally amended; binding scoping dissolves it. |
| `skills/` install path | compat §5.3: `<worktree>/.claude/skills/` + self-ignoring `.claude/.gitignore` (`*`); tracked-`.claude` conflicts refuse; missing `Skill` in the allowlist refuses with two named remedies. |
| stale §9 prose | compat §9: 53 / 13 / phase 2e throughout; §16 records the corrections. |
| component spec stale | component rev 3: header lists every override; §4/§6/§10/§14 updated (skills installed, `orders/` directory, binding-scoped decision 9, pack-level imports delegated to umbrella §7.2). |
| B1 lossy notify, no net | control-plane §2.3: "not new machinery" deleted and inverted (patrol watches, never tails); byte-offset reads drained on EVERY wake; Rescan/empty-paths = drain everything; delivery bound stated (≤ one stall interval, never lost). |
| B2 ladder SIGKILLs BLOCKED | control-plane §5.3.3: `permission.pending` disarms the stall timer; decision re-arms; the ladder's first act is always to drain the read channel (simultaneously B1's net and B2's fix). |
| B3 bounding vs tailing | control-plane §2.3 + §9: append-only until reap; `max_stream_bytes` = loud session failure; cursors are byte offsets (restart-durable for free). |
| invariant-1 evidence false | control-plane §4.3: what the empty-camp gate proves vs. doesn't, stated; obligation to extend `make perf` to M tailed workers + N subscribers; §8 test list carries it. |
| kill-on-adoption kills healthy / strands bead | control-plane §5.3 step 5 (ledger-before-pipe ordering makes pending-in-ledger proof of never-sent) + §5.3.4: named reason `"adoption: unanswerable permission request"`, bead re-hooks exactly as patrol restart; the inverse crash window degrades to the bounded ladder. |
| fail-fast refuses every agent today | control-plane §5.3.1: the stdio flag is per-agent, added only when the resolved mode can ask; F7's pinned config spawns unchanged. |
| real-claude gate mostly free | control-plane §8: split into a $0 tier (argv, initialize, pre-turn interrupt) and a paid tier riding `make e2e`. |
| subscribe needs numbers | control-plane §4.4: `subscriber_buffer_bytes` (1 MiB default, `MAX_REQUEST_BYTES` mold — `event_loop.rs:53`), drop-loudly policy, hello within `REQUEST_TIMEOUT`. |

## Verified correct — do not re-litigate

- **The control protocol.** The CLI is a full bidirectional control server on stdin/stdout: `interrupt`, `can_use_tool` (CLI→parent), `set_model`, `set_permission_mode`, `initialize`, `control_cancel_request`. `--permission-prompt-tool stdio` is special-cased **in the CLI itself**. No PTY needed. **`notify` does not degrade to polling** on either target platform (compile-time alias; FSEvents uses `latency: 0.0` + `NoDefer`).
- **File-tail-over-watch is the right transport** (piping stdout would SIGPIPE the worker on campd's death, breaking adoption).
- **`{{cmd}}` does NOT abstract the binary in prompts** — `{{ cmd }} hook` = **0**, literal `gc hook` = 151. *(Grep with spaces inside the braces; a no-space grep returns 0 and fools you.)*
- **0 of 17 gc providers have a `tools` option key**, and gc's default is `permission_mode = unrestricted`. Operator-owned `[agent_defaults]` + refusing to spawn without a resolved allowlist is correct.
- **Depth-1 pack imports cover 100% of the corpus.** Only bmad/gstack/compound-engineering/superpowers declare `[imports.*]`, all → `../gascity`; gascity declares none.
- **`hook --claim` returning `action:"drain"` is honoured**: the fragment checks `drain` at line 95, *before* the exit-code check at 101, so exit 1 on drain is harmless.
- **The shim must embed camp's absolute path** — campd's PATH is a snapshot baked at service-install and is not guaranteed to contain camp's bindir.
- The permissiveness rule and its three traps; the security analysis (§5.2, §13); the per-key formula semantics (§9); making the ladder a test rather than a boast.

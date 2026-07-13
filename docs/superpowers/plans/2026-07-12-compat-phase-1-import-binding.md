# Gas City compat phase 1 — import machinery + the binding namespace + pack loader

> **AMENDMENT_APPROVED, 2026-07-13 (adversarial retrospective plan-gate panel).** Verdict supersedes the original insufficient approval above. Source citations re-verified against `origin/main` (tip `bdae6a5`, which contains fix-81 #91, fix-83 #92, fix-86 #88 — all wave-1 merges). Eleven binding fixes applied by the implementer (defects 1–6, 9; I1, I2, I6, I10): (D1) transitive `agents/` refusal test; (D2) remote transitive source refusal test; (D3) import-time unbound-binding scan test; (D4) nested-pack report test; (D5) export test regression — `export_city.rs`/`export_golden.rs`/golden fixtures added to the owned set, `report.agents` counts immediate `agents/` subdirs, golden tree regenerated; (D6) `daemon_orders.rs` hot-reload test migrated to the import model (pre-materialize starter + `[imports.starter]` + `default_agent = "starter.dev"`); (D9) nine `tests/` files' `write_agent` helpers migrated `.md`→directory + `[agent_defaults].tools`; (I1) `camp sling --formula` rewired to `resolve_formula`; (I2) daemon fire-loop deferral made explicit + non-vacuous `disabled_imported_order_does_not_execute_fire`; (I6) namespaced imported order names bypass `valid_name` (constructed from binding+stem); (I10) `isolation = "none"` opt-out read from `agent.toml` and honored. **HOLDS (operator-bound, issue #80):** D7 (Task 21 local-path import semantics — read-in-place vs materialize-a-copy) and D8 (Task 24 direct-vs-transitive binding clash — override vs no-override) are NOT implemented pending the operator's ruling. The two sibling-owned inline tests in `patrol.rs`/`dispatch.rs` broken by the Task 10/11 contract change are held for the operator's ruling on who fixes them (the implementer does not touch wave-1 source files).

> **HOLDS RESOLVED — operator rulings, 2026-07-13 (recorded on issue #80).** Both holds above are now ruled on and implemented; the amendment is complete.
>
> - **D7 (Task 21) = READ-IN-PLACE.** A local-path import is layered in place: resolvers read from the source path, resolved relative to `camp.toml`; `run_add` skips clone, `resolve_commit`, and the lock entry for local paths; `LockEntry` stays non-`Option` (a local path has NO lock entry), matching §5's layout diagram (*"local path: layered in place / no fetch, no lock entry"*). Rationale carried into the code: `packs.lock` reproduces a fetch **by commit**, and a local path has no commit to pin — an entry with an empty commit would be a lock that reproduces nothing. Consequences fixed under the same seam: `camp import check` no longer reports a local (or transitive) import as missing, and `camp import remove` can unbind a local import — it had no lock entry to key off — while never deleting the operator's own source directory.
> - **D8 (Task 24) = DIRECT OVERRIDES TRANSITIVE.** A direct import overrides a transitive one for the same binding (§7.1; gc's own rule, `pack.go:335-340`). `imports/<binding>/` reflects the **direct** import (its agent content), while the transitive content layer persists under a **separate** path — the `.transitive` sentinel, unspellable as a binding (`[A-Za-z0-9_-]+` forbids a leading `.`) — and is scanned by **bare name**. So a direct override never clobbers or merges away the transitive **formula** layers the corpus's `extends = [...]` and `[vars]` defaults are built on. This is the §3 recipe's real clash: `bmad` imports `../gascity` transitively as `gc`, then the operator imports `gascity/roles` **directly** as `gc`.
>
> Both rulings land on ONE seam — `ImportDecl::layer_dir` + `transitive_layer_dir` — through which agent, formula, order, skills, exec-inventory, route-scan, `check` and `remove` all resolve, so the write side and the read side cannot drift. Import resolution is now driven by the `[imports.*]` **declarations** rather than by listing `imports/` (a local import has no dir there; the sentinel is not a binding).
>
> **Sibling inline tests — AUTHORIZED.** The operator authorized this stream to fix the wave-1 inline tests its own Task 10/11 contract change broke, preserving each test's original intent: `patrol::tests::frontmatter_stall_after_governs_the_armed_threshold` (the 5m override moves to `agent.toml`), `dispatch::tests::a_cap_full_patrol_respawn_queues_and_retries_when_a_slot_frees` (the `isolation = "none"` opt-out moves to `agent.toml`; cap/retry semantics untouched), plus `patrol::tests::apply_config_lets_patrol_resolve_a_reloaded_pack_agent` and `apply_config_swaps_every_config_derived_field` (the retired `packs = [...]` becomes `[imports.pack]`; the agent resolves by its qualified name; both keep their #81 guard that the birth config cannot resolve the agent, so the reload stays load-bearing).

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task (planning and execution are separate sessions, per the kickoff amendment). Steps use checkbox (`- [ ]`) syntax for tracking.

> **Plan review: APPROVE, 2026-07-13 (Opus 4.8 plan gate).** Corpus pin 44b2eef verified live; signatures/layout/§14 table verified. Two required amendments applied in this commit (yaml_rust2 retained additive-only; trust_exec transitive-inventory test added). Coordination items (a)-(c) accepted as surfaced: install_skills call-site deferred to phase 3, AgentDef signatures frozen, §5.2 sibling-test interaction lead-sequenced at rebase. *(Recorded by the planning session per the coordinator's directive; the wave-2 implementer verifies this note on its first execution commit.)*

**Goal:** A fresh camp can run a real Gas City pack. The §3 two-command recipe — `camp import add <bmad-pack> --name bmad` then `camp import add <roles-pack> --name gc` — materializes a bmad-shaped pack, its transitive `gascity` content layer, and a roles pack bound as `gc`, against LOCAL `file://` fixtures, with agents resolvable by their qualified `<binding>.<agent>` names. Fixes #80 (fresh camp, zero agents) and #85 (export round-trip) by construction; first slice of #84.

**Architecture:** New import machinery is split between camp-core (pure: source grammar, lock model, pack manifest, materialization, transitive resolution, binding-qualified agent/formula/order resolution, skills install) and the camp binary (`camp import` verbs + the single hardened git subprocess + `camp init` starter flow). The agent format changes from a Claude-Code `.md` file to a Gas City agent **directory**; `AgentDef` and `resolve_agent(&CampConfig, &str) -> Result<AgentDef, CoreError>` keep their signatures so the sibling-owned consumers (`dispatch.rs`, `patrol.rs`, `sling.rs`, `spawn.rs`) never need editing — only their resolution *source* changes. Model/permission/tools become operator-owned (`[agent_defaults]` in `camp.toml`), never pack-owned. Pack layering that was half-built in Phase 12 (`formulas/`, `orders/`) is finished here.

**Tech Stack:** Rust (workspace: `camp-core` library + `camp` binary), `toml`/`serde` with `deny_unknown_fields`, `rusqlite` SQLite ledger, `std::process::Command` for git, `tempfile` + `file://` git fixtures for tests, Python `tomllib` for the corpus gate.

## Global Constraints

Copied verbatim from AGENTS.md, the kickoff (issue #80 comment + amendment), and the two specs (`2026-07-12-gas-city-pack-compatibility-design.md` = "umbrella"; `2026-07-12-camp-pack-imports-design.md` rev 3 = "component"; where they disagree, the umbrella wins).

- **Branch:** `compat-1-import-binding`. Never commit to main. One reviewable PR. No co-author lines in commits.
- **TDD strictly:** write the failing test, run it, watch it fail, implement, watch it pass. Run every new or changed test before claiming anything. Every new test must die against a mutation of the code it guards (umbrella §14).
- **Gates before push (all must be green):** `cargo fmt --all --check` · `cargo clippy --workspace --all-targets --all-features -- -D warnings` · `cargo test --workspace`. Work is not complete until pushed and CI is green — foreground-watch `gh pr checks --watch` to the settled result; never report "CI is running".
- **No panics in library code:** clippy `unwrap_used`/`expect_used`/`panic` are denied outside `#[cfg(test)]`; `unsafe_code` forbidden. Every error surfaces to the caller or lands in the ledger. Fail fast, no fallbacks, no silenced errors, no placeholders (AGENTS.md invariant 5).
- **No network in tests:** git-backed imports run against local `file://` repos built in a temp dir (`git init`, commit a pack, clone from it). The source grammar MUST accept `file://` for this reason.
- **No API spend in tests:** no test spawns a real `claude`; workers are `#!/bin/sh` fakes. Anything needing real spend is an escalation to the lead.
- **Additive events only:** new events use `deny_unknown_fields` payload structs, keep the one-transaction event+state property, satisfy the vocab-pin partition tests, and keep the refold property test green (umbrella "respect existing interfaces").
- **`agent.toml` tolerates unknown keys** (umbrella §4): `camp.toml`'s `deny_unknown_fields` strictness must never leak into `agent.toml`, or 72/80 real agents hard-fail. Pin with a `fallback = true` regression test.
- **File ownership (kickoff PARALLEL NOTE).** OWNED by this stream: `crates/camp-core/src/pack.rs`, the new `crates/camp-core/src/import/` modules, `crates/camp-core/src/config.rs`, `crates/camp-core/src/orders/mod.rs`, `crates/camp-core/src/orders/parse.rs`, `crates/camp-core/src/error.rs`, `crates/camp/src/cmd/import.rs` (new), `crates/camp/src/cmd/order.rs`, `crates/camp/src/cmd/init.rs`, `crates/camp/src/gitignore.rs`, `packs/starter/`, `ci/gc-compat/`, `contrib/docker/`. SHARED (keep changes additive; expect small rebases): `crates/camp/src/main.rs`, `crates/camp-core/src/event.rs`, `crates/camp-core/src/vocab.rs`, `crates/camp-core/src/ledger/fold.rs`, `Cargo.toml`, `Cargo.lock`. **Do NOT touch a sibling's owned files:** `crates/camp/src/daemon/spawn.rs` (fix-82, fix-86), `crates/camp/src/daemon/patrol.rs` + `event_loop.rs` CONFIG_WATCH (fix-81), `crates/camp/src/daemon/dispatch.rs` (fix-83). After any sibling PR merges, the lead will instruct a rebase onto main + full-gate re-run before opening/updating the PR.
- **Escalate, don't invent:** a genuine spec/contract ambiguity → stop and ask the lead. A needed spec edit → stop and escalate (spec edits are serialized through the operator). Helper agents follow `.claude/skills/subagent-hygiene` (poll for results; no helper needs a permission escalation).
- **Corpus pin (this phase CREATES it):** `ci/gc-compat/GCPACKS_REF` = `44b2eef94f035283b70df62d3bd1fc77bce13d56` (see "Corpus pin decision" below). Numbers reproduced 2026-07-12 via `python3 ci/gc-compat/measure_corpus.py <clone>`.

---

## Corpus pin decision (record in the PR description too)

`ci/gc-compat/GCPACKS_REF` is pinned to **`44b2eef94f035283b70df62d3bd1fc77bce13d56`** — the tip of `gastownhall/gascity-packs` `main` on 2026-07-12 (commit dated 2026-07-09, *"fix(gastown): harden the polecat work protocol against races, duplicate work, and failed drains (#185)"*). Rationale, mirroring `GASCITY_REF`'s mold (a single commit sha, one line, moved deliberately by PR — umbrella §10):

- A git commit sha is a content pin for the **whole tree** at once: the four v1 packs (`bmad`, `gstack`, `compound-engineering`, `superpowers`), the transitive `gascity` subpath, `gascity/roles`, and the `gc-role-worker` fragment the phase-3 real-fragment test will execute. The registry-manifest-hash approach rev 3 planned cannot name 3 of the 4 v1 packs (they are not registered), so a sha is the only pin that covers them (umbrella §10).
- The corpus is **not vendored** (no top-level LICENSE; AGPL-incompatible mixed tree — umbrella §10). CI fetches at `GCPACKS_REF`; the tree is never committed.

**Numbers reproduced at this sha (must match the specs, and they do):** 100 formulas; 79 `graph.v2`, 21 none; `description_file` 53, step `metadata` 53, `condition` 13, `drain` 13; agents per pack bmad 10 / gstack 13 / compound-engineering 28 / superpowers 9 / gastown 7 / oversight-rig 1 / **gascity 0 (nested pack `roles`)**; the 4 importers (`bmad`, `gstack`, `compound-engineering`, `superpowers`) each declare exactly `[imports.gc] source = "../gascity"`; **zero bare route values, corpus-wide** (55 literal `gc.run-operator` + 46 `{{implementation_target}}` whose `[vars]` defaults are all qualified). `review-synthesizer/` exists in **both** `gstack/agents/` and `gascity/roles/agents/` (the collision that must coexist as `gstack.review-synthesizer` + `gc.review-synthesizer`). `bmad/agents/architect/agent.toml` carries `fallback = true`. `bmad/skills/bmad-create-architecture/` exists (the skill 9/10 bmad agents name).

---

## Phase boundary (what this phase does NOT do)

Umbrella §12: phase 1 is **fetch/lock/install, git hardening, `trust_exec`, `pack.toml`, pack-level `[imports.*]` transitive materialization (§7.2), binding-qualified agent resolution (§7.1), agent directories, `formulas/`/`orders/` as layered content, `skills/` install (§5.3)**. It **loads and materializes**; it runs no formulas.

Explicitly deferred (do NOT build here):
- **Formula compilation / key-set rungs / `drain`** → phase 2. No formula-compiler changes. `formulas/` and `orders/` are materialized and layered as *content*; they are not compiled or run in phase-1 tests.
- **The `gc`/`bd` shims, worker env (`BEADS_ACTOR`/`GC_*`), `hook --claim` qualified routes, `runtime drain-ack`, `.camp/bin`, the REAL-FRAGMENT test, the bead-side claim invariant, `#86 --verbose`** → phase 3. Phase 1 proves the qualified name *resolves* and that an unbound binding *fails at cook/dispatch time*; it does not stamp `gc.routed_to` on a bead or run a worker.
- **The dispatch-time skills-install call-site in `spawn.rs`** (sibling-owned) → the pure `install_skills` function + its behavior tests ship here (Task 15); wiring the call into the worker-spawn path is a phase-3 integration item, coordinated through the lead. Phase-1 acceptance is *materialization*, not dispatch.
- **`camp mail`, the control plane, `[[exports]]`, semver, the registry, credentials, `why`/`--tree`/`prune`/`status`/`migrate`, `commands/`** → later phases / out of scope (umbrella §15).

---

## File structure

**camp-core (library) — OWNED:**
- `crates/camp-core/src/config.rs` (modify): remove `packs`; add `imports: BTreeMap<String, ImportDecl>`, `orders_section: OrdersSection` (`[orders] enabled`), `agent_defaults: AgentDefaults`. A legacy `packs = [...]` key produces a specific rewrite error (component §13).
- `crates/camp-core/src/import/mod.rs` (create): the import surface + re-exports; transitive resolution graph.
- `crates/camp-core/src/import/source.rs` (create): `Source` normalization (repository + subpath + ref), pure.
- `crates/camp-core/src/import/lock.rs` (create): `PacksLock` (`schema = 1`, entries keyed by verbatim source, `{version, commit, fetched, via?}`), read/write.
- `crates/camp-core/src/import/manifest.rs` (create): `PackManifest` (`[pack] name`, `schema ≤ 2`, optional `[imports.*]`), required.
- `crates/camp-core/src/import/materialize.rs` (create): copy a subpath tree into a destination with symlinks dereferenced; dangling / repo-escape = hard error.
- `crates/camp-core/src/import/skills.rs` (create): `install_skills(pack_dir, worktree)` → `<worktree>/.claude/skills/<skill>/` + `<worktree>/.claude/.gitignore` = `*`; tracked-`.claude` conflict = hard error.
- `crates/camp-core/src/import/inventory.rs` (create): scan a materialized pack (incl. transitive) for executable content (`check.path`, `pre_start`, `condition` shell) — the `trust_exec` inventory.
- `crates/camp-core/src/pack.rs` (rewrite): agent-directory parser; `[agent_defaults]` resolution; tool-allowlist refusal; binding-qualified `resolve_agent`; formula-layer resolution (`resolve_formula`).
- `crates/camp-core/src/orders/mod.rs` + `orders/parse.rs` (modify): `formula_path` → `resolve_formula` through layers; pack orders scanned from `orders/` directories, namespaced `<import>.<order>`, INERT until `[orders] enabled` names them.
- `crates/camp-core/src/error.rs` (modify): `CoreError::Import { .. }` variant.

**camp (binary) — OWNED:**
- `crates/camp/src/cmd/import.rs` (create): `add|install|list|remove|upgrade|check` + the single hardened `git()` subprocess (argv pinned byte-for-byte).
- `crates/camp/src/cmd/order.rs` (modify): `enable`/`disable`; `ls` gains a source column + disabled state.
- `crates/camp/src/cmd/init.rs` (modify): starter-pack flow (§8), pure `decide_import()`.
- `crates/camp/src/gitignore.rs` (modify): `RUNTIME_DIRS` gains `"imports"`.
- `packs/starter/` (rewrite): directory agents + `pack.toml` + `orders/` directory (keep the symlinked formula).
- `ci/gc-compat/GCPACKS_REF` (create) + a phase-1 corpus-load gate.
- `contrib/docker/entrypoint.sh` + `compose.yaml` (modify): pass `--import "$CAMP_PACK"`, run `camp import install` before `exec campd`.

**SHARED (additive):**
- `crates/camp/src/main.rs`: `Import` subcommand; `init` gains `--import`/`--no-import`; `order` gains `enable`/`disable`.
- `crates/camp-core/src/event.rs`, `vocab.rs`, `ledger/fold.rs`: the additive import audit/refusal event(s).
- `Cargo.toml`/`Cargo.lock`: NOT touched. The shared-file rule is additive-only — `yaml_rust2` stays in the workspace manifest even after Task 10 drops its last `use` (an unused workspace dep does not fail `clippy -D warnings`). See Follow-ups.

---

## Camp layout note (get this right once — every path task depends on it)

A repo-local camp dir (`.camp/`) contains `camp.toml` + `camp.db` DIRECTLY (see `crates/camp/src/cmd/init.rs`). `cfg.root` is that camp dir. Therefore, per component §5's layout diagram, the materialized imports sit **beside** `camp.toml`: `<cfg.root>/imports/<binding>/`, and `packs.lock` is `<cfg.root>/packs.lock`. **Everywhere below, use `cfg.root.join("imports")` and `cfg.root.join("packs.lock")` — NOT `.camp/imports`.** The gitignore entry (Task 2) is anchored relative to the camp dir, which the existing `anchored_prefix` already handles (it emits `/.camp/imports/` when the camp dir is `.camp`).

---

## Task ordering and dependency map

```
Task 1  config: imports / orders.enabled / agent_defaults / packs-removal error
Task 2  gitignore: RUNTIME_DIRS += imports
Task 3  additive ledger events (import.added, import.refused)         [SHARED files]
Task 4  source normalization (pure)                    ── depends 1
Task 5  packs.lock model (pure)                        ── depends 4
Task 6  hardened git() + clone/resolve against file:// ── depends 4,5
Task 7  pack.toml manifest (pure)
Task 8  materialization + symlink deref                ── depends 7
Task 9  transitive resolution + dedupe                 ── depends 4,7,8
Task 10 agent-directory parser + §5.4 refusals         ── depends 3,7
Task 11 [agent_defaults] + tool/skill allowlist refusal ── depends 1,10
Task 12 binding-qualified resolve_agent                ── depends 1,9,10,11
Task 13 resolve_formula through layers                 ── depends 1,9
Task 14 pack orders + money invariant (enabled gate)   ── depends 1,9,13
Task 15 install_skills + self-gitignore                ── depends 7
Task 16 trust_exec inventory + default-deny            ── depends 1,9
Task 17 cmd/import verbs + reporting                   ── depends 4,5,6,7,8,9,16
Task 18 camp order enable/disable + ls                 ── depends 14
Task 19 camp init starter flow + docker               ── depends 17
Task 20 rewrite packs/starter as directory pack        ── depends 7,10
Task 21 #80 failing→passing test                       ── depends 12,17,19,20
Task 22 #85 export round-trip failing→passing test     ── depends 10,20
Task 23 GCPACKS_REF + phase-1 corpus-load gate         ── depends 9,12,13
Task 24 §3 two-command recipe end-to-end acceptance    ── depends all
```

---

## §14 obligation → task/test map (the plan's contract with the spec)

| umbrella §14 obligation (phase-1 slice) | task | named test |
|---|---|---|
| `file://` clone/lock/materialize, no network; bmad-shaped fixture with `[imports.gc] source="../gascity"`; transitive resolution, dedupe, repo-escape error | 6, 9, 24 | `import::tests::transitive_gascity_is_materialized_and_deduped`, `import::tests::relative_source_escaping_repo_root_is_hard_error`, `cmd::import::tests::add_from_file_repo_clones_locks_materializes` |
| routing: `gc.run-operator` with `gc` absent fails at cook/dispatch naming the `--name gc` remedy; present → qualified resolution | 12, 13 | `pack::tests::route_to_unbound_binding_fails_naming_remedy`, `pack::tests::qualified_route_resolves_through_binding` |
| collision: `gstack.review-synthesizer` + `gc.review-synthesizer` coexist; two agents one name **within** a binding hard-error | 12 | `pack::tests::same_name_across_bindings_coexists`, `import::tests::transitive_binding_clash_is_a_hard_error` |
| the money invariant: an imported due-cron order fires **nothing** until `[orders] enabled` names it | 14 | `orders::parse::tests::imported_order_is_inert_until_enabled`, `orders::tests::disabled_imported_order_does_not_execute_fire` |
| `trust_exec`: an imported formula's `check.path` — **including one reached through a transitive parent** — is inventoried, untrusted by default | 16, 17 | `import::inventory::tests::transitive_check_path_is_inventoried_and_untrusted_by_default` (per-dir mechanics), `cmd::import::tests::add_from_file_repo_clones_locks_materializes` (the transitive gascity fixture's `check.path` appears in the `import.added` exec inventory, `trust_exec` false; dies against the exec-vs-shell mutation) |
| §5.4 refusals are appended as ledger events, never silently skipped (umbrella §5.4) | 10, 17 | `pack::tests::unsupported_keys_are_refused_and_named` (the decision), `cmd::import::tests::add_from_file_repo_clones_locks_materializes` (one `import.refused` ledger event per refused key, naming pack/agent/key) |
| tool-allowlist refusal: no resolvable `tools` → no spawn; pack ships `skills/` but `Skill` missing → no spawn, two named remedies | 11 | `pack::tests::agent_without_resolved_tools_is_refused`, `pack::tests::skill_missing_from_allowlist_is_refused_with_remedies` |
| skills gitignore: after install + `git add -A`, `git status --porcelain` shows nothing under `.claude/` | 15 | `import::skills::tests::installed_skills_are_self_ignored_after_add` |
| `fallback = true` parses and is ignored | 10 | `pack::tests::agent_toml_tolerates_unknown_fallback_key` |
| git hardening argv asserted byte-for-byte | 6 | `cmd::import::tests::hardened_git_argv_is_exact` |
| `#80` fresh camp → zero agents, failing-then-passing | 21 | `cmd::init::tests::fresh_camp_has_no_agents_until_starter_import` |
| `#85` export round-trip, failing-then-passing | 22 | `export::tests::exported_pack_is_gc_discoverable_directory_shaped` |
| every new test dies against a mutation of the code it guards | all | verified per task in the "Mutation check" step |

---

## Task 1: Config surface — `[imports.*]`, `[orders] enabled`, `[agent_defaults]`, and the `packs` removal error

**Files:**
- Modify: `crates/camp-core/src/config.rs`
- Test: same file's `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: nothing (foundation).
- Produces:
  - `pub struct ImportDecl { pub source: String, pub subpath: Option<String>, pub version: Option<String>, pub trust_exec: bool, pub skills: Option<bool> }` (`deny_unknown_fields`). `skills = false` is the §5.3 opt-out; `trust_exec` defaults false (§13 default-deny).
  - `CampConfig.imports: BTreeMap<String, ImportDecl>` (`#[serde(default)]`, TOML `[imports.<binding>]`).
  - `CampConfig.orders_section: OrdersSection` with `pub struct OrdersSection { pub enabled: Vec<String> }` (TOML `[orders]`, `rename = "orders"`; distinct from the existing `[[order]]` = `rename = "order"`).
  - `CampConfig.agent_defaults: AgentDefaults` where `pub struct AgentDefaults { pub model: Option<String>, pub permission_mode: Option<String>, pub tools: Option<Vec<String>> }` (`deny_unknown_fields`).
  - The `packs` field is REMOVED. `CampConfig::parse` detects a `packs` key and returns a specific rewrite error.

- [ ] **Step 1: Write the failing tests** (append to `config.rs` tests)

```rust
#[test]
fn imports_orders_enabled_and_agent_defaults_parse() {
    let cfg = CampConfig::parse(
        r#"
[camp]
name = "dev"

[imports.bmad]
source = "https://github.com/gastownhall/gascity-packs"
subpath = "bmad"
version = "sha:deadbeef"

[imports.gc]
source = "../local/roles"
trust_exec = true
skills = false

[orders]
enabled = ["bmad.nightly", "gc.triage"]

[agent_defaults]
model = "sonnet"
permission_mode = "acceptEdits"
tools = ["Read", "Edit", "Bash", "Skill"]
"#,
    )
    .unwrap();
    let bmad = &cfg.imports["bmad"];
    assert_eq!(bmad.source, "https://github.com/gastownhall/gascity-packs");
    assert_eq!(bmad.subpath.as_deref(), Some("bmad"));
    assert_eq!(bmad.version.as_deref(), Some("sha:deadbeef"));
    assert!(!bmad.trust_exec);
    let gc = &cfg.imports["gc"];
    assert!(gc.trust_exec);
    assert_eq!(gc.skills, Some(false));
    assert_eq!(cfg.orders_section.enabled, vec!["bmad.nightly", "gc.triage"]);
    assert_eq!(cfg.agent_defaults.model.as_deref(), Some("sonnet"));
    assert_eq!(cfg.agent_defaults.tools.as_deref().unwrap(), ["Read", "Edit", "Bash", "Skill"]);
}

#[test]
fn legacy_packs_key_is_a_specific_rewrite_error() {
    let err = CampConfig::parse("packs = [\"packs/starter\"]\n[camp]\nname = \"d\"\n").unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("packs"), "{msg}");
    assert!(msg.contains("[imports."), "must show the rewrite: {msg}");
}

#[test]
fn agent_defaults_reject_unknown_keys() {
    assert!(CampConfig::parse("[camp]\nname=\"d\"\n[agent_defaults]\nbogus = 1\n").is_err());
}
```

- [ ] **Step 2: Run — expect FAIL.** `cargo test -p camp-core config:: 2>&1 | tail -20` (unknown fields; `packs` still parses).

- [ ] **Step 3: Implement.** In `CampConfig`: delete `pub packs: Vec<PathBuf>` and its attrs. Add fields:

```rust
#[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
pub imports: std::collections::BTreeMap<String, ImportDecl>,
#[serde(default, rename = "orders", skip_serializing_if = "OrdersSection::is_default")]
pub orders_section: OrdersSection,
#[serde(default, skip_serializing_if = "AgentDefaults::is_default")]
pub agent_defaults: AgentDefaults,
```

Define the structs:

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportDecl {
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subpath: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub trust_exec: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrdersSection {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enabled: Vec<String>,
}
impl OrdersSection { fn is_default(&self) -> bool { self.enabled.is_empty() } }

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentDefaults {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
}
impl AgentDefaults { fn is_default(&self) -> bool { *self == AgentDefaults::default() } }
```

In `CampConfig::parse`, BEFORE `toml::from_str::<CampConfig>`, guard the legacy key so a friendly error beats `deny_unknown_fields`:

```rust
let doc: toml::Value = toml::from_str(text).map_err(|e| CoreError::Config(e.to_string()))?;
if doc.get("packs").is_some() {
    return Err(CoreError::Config(
        "`packs = [...]` was removed. Rewrite each pack as an import:\n  \
         [imports.<name>]\n  source = \"<path-or-url>\"\n\
         (a local pack is an import whose source is a path — component spec §13)"
            .to_owned(),
    ));
}
```

Then update the `round_trips_through_toml` test's struct literal (drop `packs`, add the three new fields with defaults) and the two existing `packs = [...]` tests: `dispatch_and_packs_parse_with_defaults` and `defaults_do_not_pollute_serialization` — rewrite them to assert the imports surface / rewrite error instead. (The `pack.rs` tests that used `packs` are handled in Task 12.)

- [ ] **Step 4: Run — expect PASS.** `cargo test -p camp-core config:: 2>&1 | tail -20`
- [ ] **Step 5: Mutation check.** Change the guard to `doc.get("packz")`; `legacy_packs_key_...` fails. Revert.
- [ ] **Step 6: Commit.**

```bash
git add crates/camp-core/src/config.rs
git commit -m "compat: camp.toml gains [imports.*], [orders] enabled, [agent_defaults]; packs removed"
```

---

## Task 2: gitignore — `.camp/imports/` is runtime state

**Files:**
- Modify: `crates/camp/src/gitignore.rs:36` (`RUNTIME_DIRS`)
- Test: same file's tests

- [ ] **Step 1: Failing tests** (append to `gitignore.rs` tests)

```rust
#[test]
fn imports_dir_is_a_runtime_dir() {
    assert!(RUNTIME_DIRS.contains(&"imports"), "materialized imports must be gitignored");
}

#[test]
fn imports_entry_is_written_anchored_and_packs_lock_stays_tracked() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".git")).unwrap();
    let camp = dir.path().join(".camp");
    std::fs::create_dir_all(&camp).unwrap();
    ensure_camp_runtime_ignored(&camp).unwrap();
    let gi = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
    assert!(gi.contains("/.camp/imports/"), "{gi}");
    assert!(!gi.contains("packs.lock"), "packs.lock stays tracked: {gi}");
}
```

- [ ] **Step 2: Run — expect FAIL.** `cargo test -p camp gitignore:: 2>&1 | tail`
- [ ] **Step 3: Implement.** `const RUNTIME_DIRS: &[&str] = &["runs", "sessions", "worktrees", "imports"];`
- [ ] **Step 4: Run — expect PASS.**
- [ ] **Step 5: Mutation check.** Remove `"imports"`; both tests fail. Restore.
- [ ] **Step 6: Commit.**

```bash
git add crates/camp/src/gitignore.rs
git commit -m "compat: gitignore .camp/imports (materialized, camp-owned)"
```

---

## Task 3: Additive ledger events — `import.added` and `import.refused`

**Files (SHARED — additive only):**
- Modify: `crates/camp-core/src/event.rs` (enum variant, `as_str`, the all-variants array the roundtrip test iterates)
- Modify: `crates/camp-core/src/vocab.rs` (`CAMP_SPECIFIC_EVENTS`)
- Modify: `crates/camp-core/src/ledger/fold.rs` (no-op fold arms)
- Modify: `crates/camp-core/src/error.rs` (`CoreError::Import`)

**Interfaces:**
- Produces: `EventType::ImportAdded` (`"import.added"`), `EventType::ImportRefused` (`"import.refused"`). **Audit-only** (no state fold — like `campd.started`), so no ledger schema change and the one-transaction property holds trivially. gc has no import-refusal event → these are **camp-specific/additive** (invariant 7), never a redefinition.
  - `import.added` data: `{ "binding", "source", "commit", "ignored_keys": [..], "reported": [..], "exec_inventory": [{ "kind", "path", "detail" }, ..] }` — `exec_inventory` aggregates Task 16's `ExecItem`s across EVERY materialized dir (self + transitive), so the untrusted-content report is durable in the ledger, not just printed.
  - `import.refused` data: `{ "binding", "pack", "agent" | null, "key", "reason" }`.
- `CoreError::Import { binding: String, reason: String }` (mirrors the `Order` variant's shape/message discipline).

- [ ] **Step 1: Failing test** (extend `event.rs` tests)

```rust
#[test]
fn import_events_roundtrip_and_are_camp_specific() {
    assert_eq!(EventType::ImportAdded.as_str(), "import.added");
    assert_eq!(EventType::ImportRefused.as_str(), "import.refused");
    assert_eq!(EventType::parse("import.added").unwrap(), EventType::ImportAdded);
    assert_eq!(EventType::parse("import.refused").unwrap(), EventType::ImportRefused);
    assert!(crate::vocab::CAMP_SPECIFIC_EVENTS.contains(&"import.added"));
    assert!(crate::vocab::CAMP_SPECIFIC_EVENTS.contains(&"import.refused"));
    assert!(!crate::vocab::GC_MIRRORED_EVENTS.contains(&"import.added"));
}
```

- [ ] **Step 2: Run — expect FAIL.** `cargo test -p camp-core event:: vocab:: 2>&1 | tail`
- [ ] **Step 3: Implement.**
  - `event.rs`: add `ImportAdded`, `ImportRefused` to the `EventType` enum, to `as_str` (`=> "import.added"` / `=> "import.refused"`), and to the array of all variants that the roundtrip test (~line 216) and `parse` iterate (grep for the `&[EventType::...]` list).
  - `vocab.rs`: add `"import.added"`, `"import.refused"` to `CAMP_SPECIFIC_EVENTS`.
  - `fold.rs`: add `EventType::ImportAdded | EventType::ImportRefused => Ok(()),` to the match (group with `CampdStarted | CampdStopped`). Audit-only: no state mutation.
  - `error.rs`: add `#[error("import {binding:?}: {reason}")] Import { binding: String, reason: String }`.
- [ ] **Step 4: Run — expect PASS.** Then the whole camp-core suite (refold property + vocab partition must stay green): `cargo test -p camp-core 2>&1 | tail`.
- [ ] **Step 5: Mutation check.** Change `"import.added"` to `"import.add"` in `as_str`; the roundtrip test fails. Revert.
- [ ] **Step 6: Commit.**

```bash
git add crates/camp-core/src/event.rs crates/camp-core/src/vocab.rs crates/camp-core/src/ledger/fold.rs crates/camp-core/src/error.rs
git commit -m "compat: additive audit events import.added / import.refused"
```

---

## Task 4: Source normalization (pure)

**Files:**
- Create: `crates/camp-core/src/import/mod.rs` (module skeleton + `pub mod source;`)
- Create: `crates/camp-core/src/import/source.rs`
- Modify: `crates/camp-core/src/lib.rs` (`pub mod import;` — alphabetical, after `id`)
- Test: `import/source.rs` tests

**Interfaces:**
- Produces:
  ```rust
  pub struct Source { pub repository: String, pub subpath: Option<String>, pub reference: Option<String>, pub is_local_path: bool }
  pub fn normalize(source: &str, version: Option<&str>) -> Result<Source, CoreError>;
  ```
  Grammar (component decision 4/5): local path (`./`, `../`, `/abs`, bare relative) → `is_local_path`, `repository` verbatim, `version` on a local path is REJECTED; `<repo-url>//<subpath>#<ref>` (go-getter subdir marker + optional `#ref`); GitHub tree URL `.../tree/{ref}[/{path}]`; transports `https|http|ssh|git@|file` (anything else, e.g. `ext::`, rejected). The ref comes from at most one of {tree-url, `#ref`, `version`}; two that disagree → error. `file://` MUST be accepted.

- [ ] **Step 1: Failing test** (`import/source.rs`)

```rust
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn generic_form_splits_repo_subpath_ref() {
        let s = normalize("git@github.com:org/repo.git//topo#v1.0", None).unwrap();
        assert_eq!(s.repository, "git@github.com:org/repo.git");
        assert_eq!(s.subpath.as_deref(), Some("topo"));
        assert_eq!(s.reference.as_deref(), Some("v1.0"));
        assert!(!s.is_local_path);
    }
    #[test]
    fn github_tree_url_is_the_convenience_form() {
        let s = normalize("https://github.com/gastownhall/gascity-packs/tree/main/bmad", None).unwrap();
        assert_eq!(s.repository, "https://github.com/gastownhall/gascity-packs");
        assert_eq!(s.subpath.as_deref(), Some("bmad"));
        assert_eq!(s.reference.as_deref(), Some("main"));
    }
    #[test]
    fn file_url_with_subpath_and_ref_is_accepted() {
        let s = normalize("file:///tmp/repo//bmad#main", None).unwrap();
        assert_eq!(s.repository, "file:///tmp/repo");
        assert_eq!(s.subpath.as_deref(), Some("bmad"));
        assert_eq!(s.reference.as_deref(), Some("main"));
    }
    #[test]
    fn local_path_carries_no_ref_and_rejects_version() {
        let s = normalize("../packs/house", None).unwrap();
        assert!(s.is_local_path && s.repository == "../packs/house" && s.subpath.is_none() && s.reference.is_none());
        assert!(normalize("../packs/house", Some("v1")).is_err());
    }
    #[test]
    fn version_supplies_the_ref_when_the_source_omits_it() {
        assert_eq!(normalize("https://github.com/o/r", Some("sha:abc")).unwrap().reference.as_deref(), Some("sha:abc"));
    }
    #[test]
    fn conflicting_refs_are_an_error() {
        assert!(normalize("https://github.com/o/r//p#v1", Some("v2")).unwrap_err().to_string().contains("ref"));
    }
    #[test]
    fn ext_transport_is_rejected() {
        assert!(normalize("ext::sh -c whoami", None).is_err());
    }
}
```

- [ ] **Step 2: Run — expect FAIL.**
- [ ] **Step 3: Implement `normalize`.** Order: (1) local path if it starts with `.`/`/` or contains no `://`, no `git@`, and no `//` subdir marker → `is_local_path`, reject `version`. (2) Otherwise remote: strip the rightmost `#ref`; split the FIRST `//` that is not the scheme's `://` into repo + subpath; rewrite a GitHub tree URL to repo + subpath + ref. (3) Reconcile the ref among {tree-url, `#ref`, `version`}: >1 present and differing → error; equal ok. (4) Validate the transport allowlist {`https`, `http`, `ssh`, `git@`, `file`}; else error. All rejections `Err(CoreError::Import { binding: source.into(), reason })`.
- [ ] **Step 4: Run — expect PASS.**
- [ ] **Step 5: Mutation check.** Drop the conflicting-ref check; `conflicting_refs_are_an_error` fails. Revert.
- [ ] **Step 6: Commit.**

```bash
git add crates/camp-core/src/lib.rs crates/camp-core/src/import/mod.rs crates/camp-core/src/import/source.rs
git commit -m "compat: import source normalization (repo + subpath + ref), file:// supported"
```

---

## Task 5: `packs.lock` model (pure)

**Files:**
- Create: `crates/camp-core/src/import/lock.rs` (+ `pub mod lock;` in `import/mod.rs`)
- Test: `import/lock.rs` tests

**Interfaces:**
- Produces:
  ```rust
  #[serde(deny_unknown_fields)] pub struct PacksLock { pub schema: i64, #[serde(default, rename = "import")] pub imports: Vec<LockEntry> }
  #[serde(deny_unknown_fields)] pub struct LockEntry {
      pub name: String, pub source: String, pub subpath: Option<String>,
      pub version: String, pub commit: String, pub fetched: String, pub via: Option<String> }
  impl PacksLock { pub const SCHEMA: i64 = 1;
      pub fn read(path: &Path) -> Result<PacksLock, CoreError>;  // missing => {schema:1, imports:[]}
      pub fn write(&self, path: &Path) -> Result<(), CoreError>;
      pub fn entry(&self, name: &str) -> Option<&LockEntry>; }
  ```
  `location` is NEVER stored (always `<root>/imports/<name>/` — component §5, the write-anywhere hole). `read` rejects `schema != 1` naming the value.

- [ ] **Step 1: Failing test**

```rust
#[test]
fn lock_roundtrips_with_via_provenance() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("packs.lock");
    let lock = PacksLock { schema: 1, imports: vec![
        LockEntry { name: "bmad".into(), source: "https://x/repo".into(), subpath: Some("bmad".into()),
            version: "sha:abc".into(), commit: "abc".into(), fetched: "2026-07-12T00:00:00Z".into(), via: None },
        LockEntry { name: "gc".into(), source: "https://x/repo".into(), subpath: Some("gascity".into()),
            version: "sha:abc".into(), commit: "abc".into(), fetched: "2026-07-12T00:00:00Z".into(), via: Some("bmad".into()) },
    ]};
    lock.write(&p).unwrap();
    assert_eq!(PacksLock::read(&p).unwrap(), lock);
    assert_eq!(PacksLock::read(&p).unwrap().entry("gc").unwrap().via.as_deref(), Some("bmad"));
    let text = std::fs::read_to_string(&p).unwrap();
    assert!(text.contains("schema = 1") && !text.contains("location"));
}
#[test]
fn missing_lock_reads_as_empty_schema_1() {
    let dir = tempfile::tempdir().unwrap();
    let lock = PacksLock::read(&dir.path().join("packs.lock")).unwrap();
    assert!(lock.schema == 1 && lock.imports.is_empty());
}
#[test]
fn unknown_schema_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("packs.lock");
    std::fs::write(&p, "schema = 2\n").unwrap();
    assert!(PacksLock::read(&p).unwrap_err().to_string().contains("schema"));
}
```

- [ ] **Step 2: Run — expect FAIL.**
- [ ] **Step 3: Implement** `read` (missing → default; parse via `toml`; reject `schema != SCHEMA` naming the value), `write` (`toml::to_string`), `entry`. (`Serialize` skips `None` for `subpath`/`via`.)
- [ ] **Step 4: Run — expect PASS.**
- [ ] **Step 5: Mutation check.** Drop the `schema != 1` check; `unknown_schema_is_rejected` fails. Revert.
- [ ] **Step 6: Commit.**

```bash
git add crates/camp-core/src/import/mod.rs crates/camp-core/src/import/lock.rs
git commit -m "compat: packs.lock model (schema=1, via provenance, derived location)"
```

---

## Task 6: The hardened `git()` subprocess + clone/resolve against `file://`

**Files:**
- Create: `crates/camp/src/cmd/import.rs` (skeleton; `git` helpers + `testsupport::init_repo`)
- Modify: `crates/camp/src/main.rs` (`pub mod import;` under `mod cmd`) — additive
- Test: `cmd/import.rs` tests

**Interfaces:**
- Produces:
  ```rust
  pub fn hardened_git_args() -> [&'static str; 20];  // 10 -c KEY=VALUE pairs, interleaved
  pub fn git_clone(repository: &str, dest: &Path) -> anyhow::Result<()>;
  pub fn resolve_commit(repository: &str, reference: Option<&str>) -> anyhow::Result<String>; // ref -> 40-char sha
  pub(crate) mod testsupport { pub fn init_repo(dir: &std::path::Path, files: &[(&str, &str)]); }
  ```
  Every network git invocation carries the flags verbatim (umbrella §13 / component §11), argv order pinned (see the test). Sanitized env: iterate `std::env::vars()` and `.env_remove(k)` for each `k` starting with `GIT_` (do NOT `env_clear` — it drops PATH). `protocol.allow=never` + the allowlist blocks `ext::`; `core.hooksPath=/dev/null` stops cloned-repo hooks. On failure, `anyhow::bail!` naming the source + git's stderr (component §10 error table).

- [ ] **Step 1: Failing test**

```rust
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn hardened_git_argv_is_exact() {
        assert_eq!(hardened_git_args(), [
            "-c", "http.followRedirects=false",
            "-c", "protocol.allow=never",
            "-c", "protocol.https.allow=always",
            "-c", "protocol.http.allow=always",
            "-c", "protocol.ssh.allow=always",
            "-c", "protocol.git.allow=always",
            "-c", "protocol.file.allow=always",
            "-c", "core.hooksPath=/dev/null",
            "-c", "core.fsmonitor=false",
            "-c", "core.untrackedCache=false",
        ]);
    }

    #[test]
    fn clone_and_resolve_a_file_repo() {
        let src = tempfile::tempdir().unwrap();
        testsupport::init_repo(src.path(), &[("pack.toml", "[pack]\nname = \"x\"\nschema = 2\n")]);
        let url = format!("file://{}", src.path().display());
        let sha = resolve_commit(&url, Some("HEAD")).unwrap();
        assert_eq!(sha.len(), 40, "resolved a full sha: {sha}");
        let dest = tempfile::tempdir().unwrap();
        git_clone(&url, &dest.path().join("clone")).unwrap();
        assert!(dest.path().join("clone/pack.toml").exists());
    }
}
```

`testsupport::init_repo(dir, files)` runs `git init -q`, writes each file (creating parent dirs), then `git -c user.email=t@t -c user.name=t add -A` and `commit -q -m init`. Reused by Tasks 8, 17, 24.

- [ ] **Step 2: Run — expect FAIL.**
- [ ] **Step 3: Implement.** `hardened_git_args` returns the 20-token array above. `resolve_commit` runs `git <hardened> ls-remote <repository> <ref|HEAD>` (strip `GIT_*` env), parses the leading sha. `git_clone` runs `git <hardened> clone <repository> <dest>` (a full clone so subpaths/commits are present for transitive resolution). Route git stderr into `anyhow::bail!` naming the source.
- [ ] **Step 4: Run — expect PASS.**
- [ ] **Step 5: Mutation check.** Drop `-c protocol.allow=never` from the array; `hardened_git_argv_is_exact` fails. Revert.
- [ ] **Step 6: Commit.**

```bash
git add crates/camp/src/cmd/import.rs crates/camp/src/main.rs
git commit -m "compat: hardened git() subprocess (argv pinned) + file:// clone/resolve"
```

---

## Task 7: `pack.toml` manifest (pure)

**Files:**
- Create: `crates/camp-core/src/import/manifest.rs` (+ `pub mod manifest;`)
- Test: same file

**Interfaces:**
- Produces:
  ```rust
  // Top level NON-strict (gc tolerates extra top-level tables); [pack] IS strict.
  pub struct PackManifest { pub pack: PackMeta, pub imports: std::collections::BTreeMap<String, crate::config::ImportDecl> }
  #[serde(deny_unknown_fields)] pub struct PackMeta { pub name: String, pub schema: i64, pub description: Option<String>, pub version: Option<String> }
  pub fn read_manifest(pack_dir: &Path) -> Result<PackManifest, CoreError>; // missing pack.toml => "not a pack"
  ```
  `[pack].name` + `[pack].schema` (≤ 2) required; `version` NOT required (gastown ships without it — component §7.4). `[imports.*]` reuses `ImportDecl`.

- [ ] **Step 1: Failing test**

```rust
#[test]
fn reads_pack_and_optional_pack_level_imports() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("pack.toml"),
        "[pack]\nname = \"bmad\"\nversion = \"0.1.0\"\nschema = 2\n\n[imports.gc]\nsource = \"../gascity\"\n").unwrap();
    let m = read_manifest(dir.path()).unwrap();
    assert_eq!(m.pack.name, "bmad");
    assert_eq!(m.pack.schema, 2);
    assert_eq!(m.imports["gc"].source, "../gascity");
}
#[test]
fn version_is_not_required() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("pack.toml"), "[pack]\nname = \"gastown\"\nschema = 2\n").unwrap();
    assert!(read_manifest(dir.path()).is_ok());
}
#[test]
fn schema_above_2_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("pack.toml"), "[pack]\nname = \"x\"\nschema = 3\n").unwrap();
    assert!(read_manifest(dir.path()).unwrap_err().to_string().contains("schema"));
}
#[test]
fn missing_manifest_is_not_a_pack() {
    let dir = tempfile::tempdir().unwrap();
    assert!(read_manifest(dir.path()).unwrap_err().to_string().contains("pack.toml"));
}
#[test]
fn strict_pack_table_but_tolerant_top_level() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("pack.toml"), "[pack]\nname=\"x\"\nschema=2\nbogus=1\n").unwrap();
    assert!(read_manifest(dir.path()).is_err());
    std::fs::write(dir.path().join("pack.toml"), "[pack]\nname=\"x\"\nschema=2\n[catalog]\nx=1\n").unwrap();
    assert!(read_manifest(dir.path()).is_ok());
}
```

- [ ] **Step 2: Run — expect FAIL.**
- [ ] **Step 3: Implement.** `read_manifest`: no `pack.toml` → `CoreError::Import { binding: pack_dir name, reason: "no pack.toml — not a pack" }`; parse (top-level struct captures only `pack` + `imports`, with `#[serde(default)]` on `imports` and NO `deny_unknown_fields` at top level; `PackMeta` is strict); validate `schema <= 2` else named error.
- [ ] **Step 4: Run — expect PASS.**
- [ ] **Step 5: Mutation check.** Change `schema <= 2` to `<= 3`; `schema_above_2_is_rejected` fails. Revert.
- [ ] **Step 6: Commit.**

```bash
git add crates/camp-core/src/import/mod.rs crates/camp-core/src/import/manifest.rs
git commit -m "compat: pack.toml manifest (required, schema<=2, optional [imports.*])"
```

---

## Task 8: Materialization with symlink dereference

**Files:**
- Create: `crates/camp-core/src/import/materialize.rs` (+ `pub mod materialize;`)
- Test: same file

**Interfaces:**
- Produces:
  ```rust
  /// Copy `src_subtree` (a pack subpath inside a checked-out repo at `repo_root`)
  /// into `dest`, dereferencing symlinks. A symlink target escaping `repo_root`,
  /// or dangling, is a hard error (component §6/§7.4). Skips `.git`.
  pub fn materialize_tree(repo_root: &Path, src_subtree: &Path, dest: &Path) -> Result<(), CoreError>;
  ```

- [ ] **Step 1: Failing test**

```rust
#[test]
fn dereferences_symlink_inside_repo_to_a_regular_file() {
    let repo = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(repo.path().join("shared")).unwrap();
    std::fs::write(repo.path().join("shared/f.toml"), b"formula = \"x\"\n").unwrap();
    let pack = repo.path().join("packs/p/formulas");
    std::fs::create_dir_all(&pack).unwrap();
    std::os::unix::fs::symlink("../../../shared/f.toml", pack.join("g.toml")).unwrap();
    let dest = tempfile::tempdir().unwrap();
    materialize_tree(repo.path(), &repo.path().join("packs/p"), &dest.path().join("out")).unwrap();
    let out = dest.path().join("out/formulas/g.toml");
    assert!(out.is_file() && !out.is_symlink());
    assert_eq!(std::fs::read(&out).unwrap(), b"formula = \"x\"\n");
}
#[test]
fn symlink_escaping_repo_root_is_hard_error() {
    let repo = tempfile::tempdir().unwrap();
    let pack = repo.path().join("p");
    std::fs::create_dir_all(&pack).unwrap();
    std::os::unix::fs::symlink("/etc/hosts", pack.join("evil")).unwrap();
    let dest = tempfile::tempdir().unwrap();
    let err = materialize_tree(repo.path(), &pack, &dest.path().join("out")).unwrap_err();
    assert!(err.to_string().to_lowercase().contains("escape") || err.to_string().contains("repo"), "{err}");
}
#[test]
fn dangling_symlink_is_hard_error() {
    let repo = tempfile::tempdir().unwrap();
    let pack = repo.path().join("p");
    std::fs::create_dir_all(&pack).unwrap();
    std::os::unix::fs::symlink("./nope.toml", pack.join("g.toml")).unwrap();
    let dest = tempfile::tempdir().unwrap();
    assert!(materialize_tree(repo.path(), &pack, &dest.path().join("out")).is_err());
}
```

- [ ] **Step 2: Run — expect FAIL.**
- [ ] **Step 3: Implement.** Recursive copy: skip `.git`; `symlink_metadata` each entry; symlink → `std::fs::canonicalize(target)` (dereferences + resolves `..`; a nonexistent target errors → map to a dangling-link error), assert the canonical target `starts_with(repo_root.canonicalize()?)` else escape error, then copy the target's contents as a regular file; dir → recurse; file → copy bytes.
- [ ] **Step 4: Run — expect PASS.**
- [ ] **Step 5: Mutation check.** Skip the `starts_with(repo_root)` check; `symlink_escaping_repo_root_...` fails. Revert.
- [ ] **Step 6: Commit.**

```bash
git add crates/camp-core/src/import/mod.rs crates/camp-core/src/import/materialize.rs
git commit -m "compat: materialize subpath tree, symlinks dereferenced, repo-escape refused"
```

---

## Task 9: Transitive resolution + dedupe (§7.2)

**Files:**
- Modify: `crates/camp-core/src/import/mod.rs`
- Test: `import/mod.rs` tests

**Interfaces:**
- Produces:
  ```rust
  pub struct ResolvedImport {
      pub binding: String, pub source: String, pub subpath: Option<String>,
      pub reference: Option<String>, pub via: Option<String>, pub is_local: bool }
  pub fn resolve_transitive(
      direct: &[ResolvedImport],
      manifest_of: &dyn Fn(&ResolvedImport) -> Result<crate::import::manifest::PackManifest, CoreError>,
  ) -> Result<Vec<ResolvedImport>, CoreError>;
  ```
  Rules (umbrella §7.2, KNOWN-DEFECTS C3): read each direct import's `pack.toml` `[imports.*]`; a relative source anchors at the declaring pack's subpath within its own repo/commit (transitive subpath = normalized `<declaring subpath>/<relative>`, e.g. bmad `bmad/` + `../gascity` ⇒ `gascity/`); a path escaping the repo → hard error; camp materializes it ITSELF (dedupe by `(canonical repo, commit, subpath)`, `via` = declaring binding); **depth 1 enforced** (a transitive pack that itself declares `[imports.*]` → refused); a **remote** transitive source → refused (constrained to the declaring repo, umbrella §13); a transitive binding clash for a DIFFERENT `(repo, commit, subpath)` → hard error naming both; the SAME key → dedupe. (The transitive-`agents/` refusal is enforced on the materialized tree in Task 17, when the tree exists.)

- [ ] **Step 1: Failing test**

```rust
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::import::manifest::{PackManifest, PackMeta};
    fn imp(binding: &str, subpath: &str) -> ResolvedImport {
        ResolvedImport { binding: binding.into(), source: "file:///r".into(), subpath: Some(subpath.into()),
            reference: Some("c1".into()), via: None, is_local: false }
    }
    fn manifest(name: &str, gc_source: Option<&str>) -> PackManifest {
        let mut m = std::collections::BTreeMap::new();
        if let Some(s) = gc_source {
            m.insert("gc".to_string(), crate::config::ImportDecl { source: s.into(), subpath: None, version: None, trust_exec: false, skills: None });
        }
        PackManifest { pack: PackMeta { name: name.into(), schema: 2, description: None, version: None }, imports: m }
    }

    #[test]
    fn transitive_gascity_is_materialized_and_deduped() {
        let direct = vec![imp("bmad", "bmad"), imp("gstack", "gstack")];
        let mo = |i: &ResolvedImport| Ok(manifest(&i.subpath.clone().unwrap(),
            if i.subpath.as_deref()==Some("gascity") { None } else { Some("../gascity") }));
        let all = resolve_transitive(&direct, &mo).unwrap();
        let gascity: Vec<_> = all.iter().filter(|i| i.subpath.as_deref()==Some("gascity")).collect();
        assert_eq!(gascity.len(), 1, "deduped");
        assert!(gascity[0].via.is_some());
        assert_eq!(gascity[0].binding, "gc");
    }
    #[test]
    fn relative_source_escaping_repo_root_is_hard_error() {
        let direct = vec![imp("bmad", "bmad")];
        let mo = |_: &ResolvedImport| Ok(manifest("bmad", Some("../../etc")));
        assert!(resolve_transitive(&direct, &mo).unwrap_err().to_string().to_lowercase().contains("escape"));
    }
    #[test]
    fn depth_2_transitive_import_is_refused() {
        let direct = vec![imp("a", "a")];
        let mo = |i: &ResolvedImport| Ok(manifest(&i.subpath.clone().unwrap(),
            Some(if i.subpath.as_deref()==Some("a") { "../b" } else { "../c" })));
        assert!(resolve_transitive(&direct, &mo).unwrap_err().to_string().contains("depth"));
    }
    #[test]
    fn transitive_binding_clash_is_a_hard_error() {
        // two direct imports whose transitive `gc` bindings point at DIFFERENT subpaths
        let direct = vec![imp("a", "a"), imp("b", "b")];
        let mo = |i: &ResolvedImport| Ok(manifest(&i.subpath.clone().unwrap(),
            Some(if i.subpath.as_deref()==Some("a") { "../x" } else { "../y" })));
        assert!(resolve_transitive(&direct, &mo).unwrap_err().to_string().contains("gc"));
    }
}
```

- [ ] **Step 2: Run — expect FAIL.**
- [ ] **Step 3: Implement.** Start with `direct`. For each, read the manifest; for each `[imports.<b>]`: reject a remote source; compute the transitive subpath (normalize `<declaring subpath>/<relative>`, using a pure lexical normalizer — split on `/`, apply `..`, and refuse if it escapes above the repo root, i.e. a leading `..`); build the transitive `ResolvedImport` (same repo, same `reference`, `via = Some(declaring binding)`). Read the transitive manifest: if it declares `[imports.*]` → depth error. Dedupe by `(repo, reference, subpath)`. A binding used by two DIFFERENT `(repo, reference, subpath)` → hard error naming both. Return direct + deduped transitive.
- [ ] **Step 4: Run — expect PASS.**
- [ ] **Step 5: Mutation check.** Remove the dedupe; `transitive_gascity_is_materialized_and_deduped` sees 2 and fails. Revert.
- [ ] **Step 6: Commit.**

```bash
git add crates/camp-core/src/import/mod.rs
git commit -m "compat: depth-1 transitive import resolution, anchored + deduped with via"
```

---

## Task 10: Agent-directory parser + §5.4 refusals

**Files:**
- Rewrite: `crates/camp-core/src/pack.rs` (the parser half; keep `AgentDef`/`Isolation`)
- Test: `pack.rs` tests (replace the `.md` tests)

**Interfaces:**
- Produces:
  ```rust
  pub struct AgentRefusal { pub agent: String, pub key: String } // umbrella §5.4
  pub struct RawAgent { pub name: String, pub prompt: String, pub scope: Option<String>, pub stall_after: Option<String> }
  /// Parse one agent DIRECTORY (umbrella §5.1). Identity = dir name. prompt
  /// precedence: prompt.template.md, prompt.md.tmpl, prompt.md. agent.toml is
  /// OPTIONAL, unknown keys TOLERATED (umbrella §4). Returns any §5.4 refusals.
  pub fn parse_agent_dir(dir: &Path) -> Result<(RawAgent, Vec<AgentRefusal>), CoreError>;
  ```
  §5.4 refused keys (collected, not thrown — the agent still materializes; the operator is told): `pre_start`, `work_dir`, `wake_mode`, `idle_timeout`, `min_active_sessions`, `max_active_sessions`, `nudge`, `sleep_after_idle`, `max_session_age`, `max_session_age_jitter`. Model/permission/tools are NOT read from the pack (§5.2). `stall_after` validated via `crate::patrol::parse_duration`.

- [ ] **Step 1: Failing test** (delete the `.md`-era tests: `parses_a_claude_code_agent_file`, `tools_accepts_a_yaml_list...`, `isolation_none_is_an_accepted...`, `isolation_defaults_to_worktree...`, `unknown_keys_are_tolerated...`, `malformed_files_fail...`; keep `parse_duration`-related intent in the new tests)

```rust
fn write_agent_dir(root: &Path, name: &str, agent_toml: Option<&str>, prompt_file: &str, prompt: &str) {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    if let Some(t) = agent_toml { std::fs::write(dir.join("agent.toml"), t).unwrap(); }
    std::fs::write(dir.join(prompt_file), prompt).unwrap();
}
#[test]
fn agent_toml_tolerates_unknown_fallback_key() {
    let dir = tempfile::tempdir().unwrap();
    write_agent_dir(dir.path(), "architect",
        Some("description = \"BMAD architecture planner\"\nscope = \"rig\"\nfallback = true\n"),
        "prompt.template.md", "You are the architect. {{.Var}}");
    let (agent, refusals) = parse_agent_dir(&dir.path().join("architect")).unwrap();
    assert_eq!(agent.name, "architect");
    assert_eq!(agent.scope.as_deref(), Some("rig"));
    assert!(refusals.is_empty(), "fallback is ignored, not refused");
}
#[test]
fn prompt_precedence_prefers_template_md() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::write(a.join("prompt.md"), "plain").unwrap();
    std::fs::write(a.join("prompt.template.md"), "templated").unwrap();
    assert_eq!(parse_agent_dir(&a).unwrap().0.prompt, "templated");
}
#[test]
fn identity_is_the_directory_name_not_a_field() {
    let dir = tempfile::tempdir().unwrap();
    write_agent_dir(dir.path(), "run-operator", Some("name = \"something-else\"\n"), "prompt.md", "operate");
    assert_eq!(parse_agent_dir(&dir.path().join("run-operator")).unwrap().0.name, "run-operator");
}
#[test]
fn unsupported_keys_are_refused_and_named() {
    let dir = tempfile::tempdir().unwrap();
    write_agent_dir(dir.path(), "pooled",
        Some("work_dir = \"x\"\nmax_active_sessions = 3\npre_start = \"boot\"\n"), "prompt.md", "p");
    let (_a, refusals) = parse_agent_dir(&dir.path().join("pooled")).unwrap();
    let keys: std::collections::BTreeSet<_> = refusals.iter().map(|r| r.key.as_str()).collect();
    assert!(keys.contains("work_dir") && keys.contains("max_active_sessions") && keys.contains("pre_start"), "{keys:?}");
    assert!(refusals.iter().all(|r| r.agent == "pooled"));
}
#[test]
fn missing_prompt_is_a_hard_error() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::write(a.join("agent.toml"), "scope=\"rig\"\n").unwrap();
    assert!(parse_agent_dir(&a).unwrap_err().to_string().contains("prompt"));
}
```

- [ ] **Step 2: Run — expect FAIL.**
- [ ] **Step 3: Implement.** `name` = `dir.file_name()`. Read `agent.toml` (if present) as `toml::Value`; extract `scope`/`stall_after` (validate `stall_after`); collect an `AgentRefusal` for each present §5.4 key. Prompt: first existing of `prompt.template.md`, `prompt.md.tmpl`, `prompt.md`; else hard error naming `prompt`; empty → hard error. Delete `parse_agent_file`, the `use yaml_rust2::...` line in `pack.rs` (owned file), and the deleted tests. **Do NOT remove `yaml_rust2` from `Cargo.toml`/`Cargo.lock`** — the shared-file rule is additive-only, and an unused workspace dep does not fail `clippy -D warnings`; its removal is a deferred follow-up (see Follow-ups).
- [ ] **Step 4: Run — expect PASS.**
- [ ] **Step 5: Mutation check.** Prefer `prompt.md` over `prompt.template.md`; `prompt_precedence_prefers_template_md` fails. Revert.
- [ ] **Step 6: Commit.**

```bash
git add crates/camp-core/src/pack.rs
git commit -m "compat: agent directories replace .md files; §5.4 unsupported keys refused + named"
```

---

## Task 11: `[agent_defaults]` resolution + tool/skill allowlist refusal (§5.2, §5.3)

**Files:**
- Modify: `crates/camp-core/src/pack.rs`
- Test: `pack.rs` tests

**Interfaces:**
- Consumes: `RawAgent` (Task 10), `AgentDefaults` (Task 1).
- Produces:
  ```rust
  pub fn resolve_agent_def(defaults: &AgentDefaults, raw: &RawAgent, qualified_name: &str, pack_ships_skills: bool)
      -> Result<AgentDef, CoreError>;
  ```
  `AgentDef` keeps its EXISTING fields (`name`, `model`, `tools`, `permission_mode`, `isolation`, `stall_after`, `prompt`) so `spawn.rs` is untouched. `model`/`permission_mode`/`tools` come ONLY from `defaults`. No resolvable `tools` → error. `pack_ships_skills && Skill ∉ tools` → error naming `Skill` + both remedies. `isolation` = `Worktree`.

- [ ] **Step 1: Failing test**

```rust
fn defaults(tools: Option<Vec<&str>>) -> AgentDefaults {
    AgentDefaults { model: Some("sonnet".into()), permission_mode: Some("acceptEdits".into()),
        tools: tools.map(|v| v.iter().map(|s| s.to_string()).collect()) }
}
fn raw(name: &str) -> RawAgent { RawAgent { name: name.into(), prompt: "p".into(), scope: None, stall_after: None } }
#[test]
fn agent_def_takes_model_permission_tools_from_operator_defaults() {
    let def = resolve_agent_def(&defaults(Some(vec!["Read","Edit","Bash"])), &raw("architect"), "bmad.architect", false).unwrap();
    assert_eq!(def.name, "bmad.architect");
    assert_eq!(def.model.as_deref(), Some("sonnet"));
    assert_eq!(def.permission_mode.as_deref(), Some("acceptEdits"));
    assert_eq!(def.tools.as_deref().unwrap(), ["Read","Edit","Bash"]);
}
#[test]
fn agent_without_resolved_tools_is_refused() {
    let m = resolve_agent_def(&defaults(None), &raw("architect"), "bmad.architect", false).unwrap_err().to_string();
    assert!(m.contains("tools") && m.contains("agent_defaults"), "{m}");
}
#[test]
fn skill_missing_from_allowlist_is_refused_with_remedies() {
    let m = resolve_agent_def(&defaults(Some(vec!["Read","Edit"])), &raw("architect"), "bmad.architect", true).unwrap_err().to_string();
    assert!(m.contains("Skill") && m.contains("skills = false") && m.contains("[agent_defaults]"), "{m}");
}
#[test]
fn skill_present_allows_a_skills_pack() {
    assert!(resolve_agent_def(&defaults(Some(vec!["Read","Skill"])), &raw("architect"), "bmad.architect", true).is_ok());
}
```

- [ ] **Step 2: Run — expect FAIL.**
- [ ] **Step 3: Implement.** `tools = defaults.tools.clone().ok_or_else(|| CoreError::Pack("agent \"<qn>\": no tool allowlist resolves — set [agent_defaults].tools in camp.toml (camp never inherits gc's unrestricted default)".into()))?;` If `pack_ships_skills && !tools.iter().any(|t| t == "Skill")` → `CoreError::Pack` naming `Skill` + *"add `Skill` to `[agent_defaults].tools`, or set `skills = false` on the import"*. Build `AgentDef { name: qualified_name.into(), model: defaults.model.clone(), permission_mode: defaults.permission_mode.clone(), tools: Some(tools), isolation: Isolation::Worktree, stall_after: raw.stall_after.clone(), prompt: raw.prompt.clone() }`.
- [ ] **Step 4: Run — expect PASS.**
- [ ] **Step 5: Mutation check.** Make missing tools default to `Some(vec![])`; `agent_without_resolved_tools_is_refused` fails. Revert.
- [ ] **Step 6: Commit.**

```bash
git add crates/camp-core/src/pack.rs
git commit -m "compat: model/permission/tools operator-owned; refuse spawn without a resolved allowlist"
```

---

## Task 12: Binding-qualified `resolve_agent` (§7.1)

**Files:**
- Modify: `crates/camp-core/src/pack.rs` (rewrite `layers`/`resolve_agent`; drop the `cfg.packs` machinery)
- Modify: `crates/camp-core/src/config.rs` if any `packs`-referencing test remains
- Test: `pack.rs` tests

**Interfaces:**
- Consumes: `resolve_agent_def`, `parse_agent_dir`, `CampConfig.imports` + `.root`, materialized `<root>/imports/<binding>/`.
- Produces (signature UNCHANGED): `pub fn resolve_agent(cfg: &CampConfig, name: &str) -> Result<AgentDef, CoreError>;`
  Behavior (umbrella §7.1): split at the FIRST dot; prefix = a binding in `cfg.imports` (else fail-fast naming the binding + `camp import add <source> --name <binding>`); suffix = `<root>/imports/<binding>/agents/<suffix>/` (missing → `UnknownAgent`). A no-dot name → `<root>/agents/<name>/` (bare, disjoint). `gstack.review-synthesizer` + `gc.review-synthesizer` coexist by construction. `pack_ships_skills` = `<root>/imports/<binding>/skills/` exists AND the import's `skills != Some(false)`.

- [ ] **Step 1: Failing test** (delete the `packs`-era `resolve_agent_layers_packs_last_wins...`, `duplicate_agent_names_in_one_layer...`, `missing_pack_dir_is_a_hard_error...`)

```rust
fn camp_with_imports(kv: &[(&str, &str)]) -> (tempfile::TempDir, CampConfig) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let mut toml = String::from("[camp]\nname=\"t\"\n[agent_defaults]\ntools=[\"Read\"]\n");
    for (binding, _agent) in kv { toml.push_str(&format!("[imports.{binding}]\nsource=\"file:///unused\"\n")); }
    for (binding, agent) in kv {
        let a = root.join("imports").join(binding).join("agents").join(agent);
        std::fs::create_dir_all(&a).unwrap();
        std::fs::write(a.join("prompt.md"), format!("I am {binding}.{agent}")).unwrap();
    }
    std::fs::write(root.join("camp.toml"), &toml).unwrap();
    let cfg = CampConfig::load(&root.join("camp.toml")).unwrap();
    (dir, cfg)
}
#[test]
fn qualified_route_resolves_through_binding() {
    let (_d, cfg) = camp_with_imports(&[("gc","run-operator")]);
    let def = resolve_agent(&cfg, "gc.run-operator").unwrap();
    assert_eq!(def.name, "gc.run-operator");
    assert!(def.prompt.contains("gc.run-operator"));
}
#[test]
fn route_to_unbound_binding_fails_naming_remedy() {
    let (_d, cfg) = camp_with_imports(&[("gc","run-operator")]);
    let m = resolve_agent(&cfg, "bmad.architect").unwrap_err().to_string();
    assert!(m.contains("bmad") && m.contains("camp import add") && m.contains("--name bmad"), "{m}");
}
#[test]
fn same_name_across_bindings_coexists() {
    let (_d, cfg) = camp_with_imports(&[("gstack","review-synthesizer"),("gc","review-synthesizer")]);
    assert!(resolve_agent(&cfg, "gstack.review-synthesizer").unwrap().prompt.contains("gstack"));
    assert!(resolve_agent(&cfg, "gc.review-synthesizer").unwrap().prompt.contains("gc"));
}
#[test]
fn bare_name_resolves_a_camp_local_agent() {
    let (_d, cfg) = camp_with_imports(&[]);
    let a = cfg.root.clone().unwrap().join("agents/dev");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::write(a.join("prompt.md"), "local dev").unwrap();
    assert_eq!(resolve_agent(&cfg, "dev").unwrap().name, "dev");
}
```

- [ ] **Step 2: Run — expect FAIL.**
- [ ] **Step 3: Implement.** Rewrite `resolve_agent`: `let need_root = ...;` `match name.split_once('.') { Some((binding, agent)) => { cfg.imports.get(binding).ok_or_else(|| CoreError::UnknownAgent-or-Config naming remedy)?; let dir = cfg.root...?.join("imports").join(binding).join("agents").join(agent); parse_agent_dir(&dir) → resolve_agent_def(&cfg.agent_defaults, &raw, name, pack_ships_skills) } None => { let dir = cfg.root...?.join("agents").join(name); ... } }`. `pack_ships_skills` = `cfg.root...join("imports").join(binding).join("skills").is_dir() && cfg.imports[binding].skills != Some(false)`. Prefer a dedicated `CoreError` for the unbound-binding remedy (a `Config`/`Pack` string carrying `camp import add <source> --name <binding>`). Delete `layers`, `load_layer`, and all `cfg.packs` references.
- [ ] **Step 4: Run — expect PASS.** `cargo test -p camp-core pack:: 2>&1 | tail`
- [ ] **Step 5: Mutation check.** Make the unbound case return a bare `UnknownAgent` (no remedy); `route_to_unbound_binding_fails_naming_remedy` fails on the `camp import add` assertion. Revert.
- [ ] **Step 6: Commit.**

```bash
git add crates/camp-core/src/pack.rs crates/camp-core/src/config.rs
git commit -m "compat: binding-qualified agent resolution; unbound binding = named remedy; cross-binding coexist"
```

---

## Task 13: `resolve_formula` through layers (finish Phase 12 formula layering)

**Files:**
- Modify: `crates/camp-core/src/orders/mod.rs` (`formula_path` → `resolve_formula`)
- Modify: `crates/camp-core/src/export.rs` (the `formula_path` caller at line 633)
- Test: `orders/mod.rs` tests

**Interfaces:**
- Produces:
  ```rust
  /// Resolve a formula file by BARE name through layers, lowest→highest:
  /// each transitive import's formulas/, each direct import's formulas/, then
  /// <camp>/formulas/ (highest). Cross-import duplication is a hard error
  /// naming both providers. Returns the resolved path (callers read a path).
  pub fn resolve_formula(cfg: &CampConfig, name: &str) -> Result<PathBuf, CoreError>;
  ```
  Replaces `formula_path(camp_root, name)`. Callers to update: `orders::execute_fire` (has `config` in scope) and `export.rs::write_pack` (has `config` in scope). **Phase-1 scope:** resolves the path for layering; does NOT compile `extends`/`drain` (phase 2).

- [ ] **Step 1: Failing test** (add to `orders/mod.rs` tests; replace `formula_path_is_the_camp_local_formulas_dir`)

```rust
fn camp_with_formula_layers() -> (tempfile::TempDir, crate::config::CampConfig) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join("camp.toml"), "[camp]\nname=\"t\"\n[imports.bmad]\nsource=\"file:///x\"\n").unwrap();
    let f = root.join("imports/bmad/formulas");
    std::fs::create_dir_all(&f).unwrap();
    std::fs::write(f.join("build.toml"), "formula = \"imported-build\"\n").unwrap();
    std::fs::create_dir_all(root.join("formulas")).unwrap();
    std::fs::write(root.join("formulas/build.toml"), "formula = \"local-build\"\n").unwrap();
    let cfg = crate::config::CampConfig::load(&root.join("camp.toml")).unwrap();
    (dir, cfg)
}
#[test]
fn local_formula_shadows_an_imported_one() {
    let (_d, cfg) = camp_with_formula_layers();
    let p = resolve_formula(&cfg, "build").unwrap();
    assert!(!p.to_string_lossy().contains("imports"), "{}", p.display());
    assert_eq!(std::fs::read_to_string(&p).unwrap().trim(), "formula = \"local-build\"");
}
#[test]
fn an_imported_formula_is_reachable_without_a_local_override() {
    let (_d, cfg) = camp_with_formula_layers();
    std::fs::remove_file(cfg.root.as_ref().unwrap().join("formulas/build.toml")).unwrap();
    assert!(resolve_formula(&cfg, "build").unwrap().to_string_lossy().contains("imports/bmad/formulas"));
}
#[test]
fn cross_import_formula_collision_is_a_hard_error() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join("camp.toml"),
        "[camp]\nname=\"t\"\n[imports.a]\nsource=\"file:///x\"\n[imports.b]\nsource=\"file:///y\"\n").unwrap();
    for b in ["a","b"] {
        let f = root.join("imports").join(b).join("formulas");
        std::fs::create_dir_all(&f).unwrap();
        std::fs::write(f.join("dup.toml"), "formula = \"x\"\n").unwrap();
    }
    let cfg = crate::config::CampConfig::load(&root.join("camp.toml")).unwrap();
    let err = resolve_formula(&cfg, "dup").unwrap_err().to_string();
    assert!(err.contains("a") && err.contains("b"), "{err}");
}
```

- [ ] **Step 2: Run — expect FAIL.**
- [ ] **Step 3: Implement `resolve_formula`.** Build layers: for each import (direct + transitive, stable order), `<root>/imports/<binding>/formulas/<name>.toml`; then `<root>/formulas/<name>.toml` (highest). At the IMPORT tier, if `<name>` exists under two different bindings → hard error naming both. A local override wins. No hit → error naming the formula + searched layers. Update `execute_fire` (`orders/mod.rs:370` area) to `resolve_formula(config, &order.formula)` and `export.rs:633` to `resolve_formula(config, &formula)`. Delete `formula_path`.
- [ ] **Step 4: Run — expect PASS.** `cargo test -p camp-core orders:: export:: 2>&1 | tail`
- [ ] **Step 5: Mutation check.** Put `<camp>/formulas` at the BOTTOM; `local_formula_shadows_an_imported_one` fails. Revert.
- [ ] **Step 6: Commit.**

```bash
git add crates/camp-core/src/orders/mod.rs crates/camp-core/src/pack.rs crates/camp-core/src/export.rs
git commit -m "compat: resolve_formula through import+local layers (finishes Phase 12 layering)"
```

---

## Task 14: Pack orders + the money invariant (the `enabled` gate)

**Files:**
- Modify: `crates/camp-core/src/orders/parse.rs`
- Test: `orders/parse.rs` + `orders/mod.rs` tests

**Interfaces:**
- Consumes: `CampConfig.orders_section.enabled` (Task 1), materialized `<root>/imports/<binding>/orders/*.toml`, `resolve_formula` (Task 13).
- Produces:
  ```rust
  pub struct OrderInventory { pub active: Vec<Order>, pub disabled: Vec<DisabledOrder> }
  pub struct DisabledOrder { pub name: String, pub source: String /* binding */, pub formula: String }
  pub fn compile_all_orders(cfg: &CampConfig) -> Result<OrderInventory, CoreError>;
  ```
  gc derives an order's name from the FILENAME → imported order name `<binding>.<stem>`. An imported order naming an unresolvable formula → hard error at load. Camp-local `[[order]]` keep bare names + stay ACTIVE. **Only** `[orders] enabled` arms an imported order (the pinned money test).

- [ ] **Step 1: Failing test** (in `orders/parse.rs` tests; expose a `pub(crate)` helper for the mod-level test)

```rust
pub(crate) fn camp_with_imported_order(enabled: &[&str]) -> (tempfile::TempDir, CampConfig) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let mut toml = String::from("[camp]\nname=\"t\"\n\n[[rigs]]\nname=\"gc\"\npath=\"/p\"\nprefix=\"gc\"\n\n[imports.bmad]\nsource=\"file:///x\"\n");
    if !enabled.is_empty() {
        toml.push_str(&format!("\n[orders]\nenabled = [{}]\n",
            enabled.iter().map(|e| format!("\"{e}\"")).collect::<Vec<_>>().join(", ")));
    }
    std::fs::write(root.join("camp.toml"), &toml).unwrap();
    let od = root.join("imports/bmad/orders");
    std::fs::create_dir_all(&od).unwrap();
    std::fs::write(od.join("nightly.toml"), "[order]\nformula = \"nightly-formula\"\ntrigger = \"cron\"\nschedule = \"0 2 * * *\"\n").unwrap();
    let fd = root.join("imports/bmad/formulas");
    std::fs::create_dir_all(&fd).unwrap();
    std::fs::write(fd.join("nightly-formula.toml"), "formula = \"nightly-formula\"\n").unwrap();
    let cfg = CampConfig::load(&root.join("camp.toml")).unwrap();
    (dir, cfg)
}
#[test]
fn imported_order_is_inert_until_enabled() {
    let (_d, cfg) = camp_with_imported_order(&[]);
    let inv = compile_all_orders(&cfg).unwrap();
    assert!(inv.active.iter().all(|o| o.name != "bmad.nightly"), "unenabled → NOT active");
    assert!(inv.disabled.iter().any(|d| d.name == "bmad.nightly" && d.source == "bmad"), "disabled with source");
}
#[test]
fn enabling_arms_exactly_the_named_import_order() {
    let (_d, cfg) = camp_with_imported_order(&["bmad.nightly"]);
    let inv = compile_all_orders(&cfg).unwrap();
    assert!(inv.active.iter().any(|o| o.name == "bmad.nightly"));
    assert!(inv.disabled.iter().all(|d| d.name != "bmad.nightly"));
}
```

Plus, in `orders/mod.rs` tests, the money invariant test that can FAIL:

```rust
#[test]
fn disabled_imported_order_does_not_execute_fire() {
    // The fire loop only iterates the ACTIVE set. A disabled imported order is
    // never in it, so execute_fire is unreachable for it — assert active is empty.
    let (dir, cfg) = crate::orders::parse::tests::camp_with_imported_order(&[]);
    let inv = crate::orders::parse::compile_all_orders(&cfg).unwrap();
    assert!(inv.active.is_empty(), "no active order ⇒ execute_fire never reachable");
    let _ = dir;
}
```

- [ ] **Step 2: Run — expect FAIL.**
- [ ] **Step 3: Implement `compile_all_orders`.** `active` starts from `compile_orders(cfg)` (local). For each import, scan `<root>/imports/<binding>/orders/*.toml`; parse each gc order file (`[order]`: `formula`, `trigger`, `schedule`/`on`) via a small `Deserialize` struct (mirror `export::GcOrder`) into an `Order` named `<binding>.<stem>`; verify the formula resolves via `resolve_formula` (else hard error naming order + formula); if `cfg.orders_section.enabled.contains(&name)` → `active`, else → `disabled`.
- [ ] **Step 4: Run — expect PASS.**
- [ ] **Step 5: Mutation check.** Make the gate ignore `enabled` (always active); `imported_order_is_inert_until_enabled` + `disabled_imported_order_does_not_execute_fire` fail. Revert.
- [ ] **Step 6: Commit.**

```bash
git add crates/camp-core/src/orders/parse.rs crates/camp-core/src/orders/mod.rs
git commit -m "compat: pack orders scanned + namespaced, inert until [orders] enabled (money invariant)"
```

---

## Task 15: `install_skills` + the self-ignoring `.claude/`

**Files:**
- Create: `crates/camp-core/src/import/skills.rs` (+ `pub mod skills;`)
- Test: same file (real `git init` worktree)

**Interfaces:**
- Produces:
  ```rust
  /// Install a pack's skills/ into a session worktree (umbrella §5.3):
  ///   <worktree>/.claude/skills/<skill>/...    from <pack_dir>/skills/
  ///   <worktree>/.claude/.gitignore = "*\n"     (self-ignoring)
  /// Refuses LOUDLY if the worktree TRACKS .claude/.gitignore, or a tracked
  /// file collides with a skill. Returns the number of skills installed.
  pub fn install_skills(pack_dir: &Path, worktree: &Path) -> Result<usize, CoreError>;
  ```

- [ ] **Step 1: Failing test**

```rust
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::process::Command;
    fn git(dir: &Path, args: &[&str]) {
        assert!(Command::new("git").arg("-C").arg(dir).args(["-c","user.email=t@t","-c","user.name=t"])
            .args(args).status().unwrap().success(), "git {args:?}");
    }
    fn pack_with_skill(root: &Path) -> std::path::PathBuf {
        let p = root.join("pack");
        std::fs::create_dir_all(p.join("skills/bmad-create-architecture")).unwrap();
        std::fs::write(p.join("skills/bmad-create-architecture/SKILL.md"), "# skill").unwrap();
        p
    }
    #[test]
    fn installed_skills_are_self_ignored_after_add() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        git(&wt, &["init", "-q"]);
        std::fs::write(wt.join("file.txt"), "work").unwrap();
        let n = install_skills(&pack_with_skill(dir.path()), &wt).unwrap();
        assert_eq!(n, 1);
        assert!(wt.join(".claude/skills/bmad-create-architecture/SKILL.md").exists());
        assert_eq!(std::fs::read_to_string(wt.join(".claude/.gitignore")).unwrap(), "*\n");
        git(&wt, &["add", "-A"]);
        let out = Command::new("git").arg("-C").arg(&wt).args(["status","--porcelain"]).output().unwrap();
        let s = String::from_utf8(out.stdout).unwrap();
        assert!(!s.contains(".claude/"), "nothing under .claude/ staged: {s:?}");
        assert!(s.contains("file.txt"), "real work still staged: {s:?}");
    }
    #[test]
    fn tracked_dot_claude_gitignore_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("wt");
        std::fs::create_dir_all(wt.join(".claude")).unwrap();
        git(&wt, &["init", "-q"]);
        std::fs::write(wt.join(".claude/.gitignore"), "custom\n").unwrap();
        git(&wt, &["add", "-A"]);
        git(&wt, &["commit", "-q", "-m", "track"]);
        assert!(install_skills(&pack_with_skill(dir.path()), &wt).is_err());
    }
}
```

- [ ] **Step 2: Run — expect FAIL.**
- [ ] **Step 3: Implement.** No `<pack_dir>/skills/` → `Ok(0)`. `git -C <worktree> ls-files --error-unmatch .claude/.gitignore` exits 0 (tracked) → refuse. For each skill, check tracked collisions via `git -C <worktree> ls-files --error-unmatch .claude/skills/<name>` → refuse. Copy the skills tree into `<worktree>/.claude/skills/`; write `<worktree>/.claude/.gitignore` = `"*\n"`. Return the count. (Local `git ls-files`, no untrusted URL — plain `git` is fine.)
- [ ] **Step 4: Run — expect PASS.**
- [ ] **Step 5: Mutation check.** Write `.claude/.gitignore` = `"# nothing\n"`; `installed_skills_are_self_ignored_after_add` fails (skills staged). Revert.
- [ ] **Step 6: Commit.**

```bash
git add crates/camp-core/src/import/mod.rs crates/camp-core/src/import/skills.rs
git commit -m "compat: install pack skills into <worktree>/.claude/skills, self-ignored; tracked-.claude refuses"
```

> **Integration note (phase 3, coordinate with lead):** the dispatch-time call to `install_skills` lives in `crates/camp/src/daemon/spawn.rs` (sibling-owned; gc pack agents are dispatch-only, with worker env/shims in phase 3). Phase 1 ships the function + tests; the wiring is a phase-3 integration item.

---

## Task 16: `trust_exec` inventory + default-deny

**Files:**
- Create: `crates/camp-core/src/import/inventory.rs` (+ `pub mod inventory;`)
- Test: same file

**Interfaces:**
- Produces:
  ```rust
  pub struct ExecItem { pub kind: &'static str, pub path: String, pub detail: String }
  pub fn inventory_executable(pack_dir: &Path) -> Result<Vec<ExecItem>, CoreError>;
  ```
  Scans a materialized pack for executable content: formula `check.path` (when `check.mode == "exec"`), `pre_start`, `condition` shell; `exec`-triggered orders. **Phase-1 scope:** phase 1 runs NO formulas, so "executes nothing" holds by the absence of an execution path; the deliverable is the *inventory* (for `camp import add` to print and record in `import.added`) + `ImportDecl.trust_exec` default-false (Task 1). **Transitive coverage (§14.10, plan-gate required amendment):** the caller (Task 17) runs `inventory_executable` on EVERY materialized dir, including the transitive `gc` one — and Task 17's end-to-end test places a `check.path` in the transitively-imported gascity fixture and asserts it appears in the `import.added` exec inventory with `trust_exec` still false. That assertion flows through this function's `mode == "exec"` filter, so it dies against the same exec-vs-shell mutation as the unit test below.

- [ ] **Step 1: Failing test**

```rust
#[test]
fn transitive_check_path_is_inventoried_and_untrusted_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let pack = dir.path().join("bmad");
    std::fs::create_dir_all(pack.join("formulas")).unwrap();
    std::fs::write(pack.join("formulas/build.toml"),
        "formula=\"b\"\n[[steps]]\nid=\"s\"\ntitle=\"t\"\n[steps.check]\nmode=\"exec\"\npath=\"scripts/verify.sh\"\n").unwrap();
    let items = inventory_executable(&pack).unwrap();
    assert!(items.iter().any(|i| i.kind == "check.path" && i.detail.contains("verify.sh")), "{items:?}");
    let decl = crate::config::ImportDecl { source: "x".into(), subpath: None, version: None, trust_exec: false, skills: None };
    assert!(!decl.trust_exec, "untrusted unless the operator opts in");
}
```

- [ ] **Step 2: Run — expect FAIL.**
- [ ] **Step 3: Implement.** Walk `<pack_dir>/formulas/*.toml`; parse as `toml::Value`; for each step collect `steps.check.path` when `steps.check.mode == "exec"`, plus `pre_start` and `condition` (shell). Scan `<pack_dir>/orders/*.toml` for `exec`-triggered orders. Return `ExecItem`s.
- [ ] **Step 4: Run — expect PASS.**
- [ ] **Step 5: Mutation check.** Collect on `mode == "shell"` instead of `"exec"`; the test fails. Revert.
- [ ] **Step 6: Commit.**

```bash
git add crates/camp-core/src/import/mod.rs crates/camp-core/src/import/inventory.rs
git commit -m "compat: trust_exec inventory of executable pack content; default deny"
```

---

## Task 17: `camp import` verbs + reporting

**Files:**
- Modify: `crates/camp/src/cmd/import.rs` (the orchestration + verbs)
- Modify: `crates/camp/src/main.rs` (SHARED): `Import` subcommand + dispatch
- Test: `cmd/import.rs` tests (`file://` end-to-end)

**Interfaces:**
- Consumes: all of Tasks 4–9, 16, plus `parse_agent_dir` refusals, and the ledger.
- Produces (component §9): `run_add(camp_root, source, name, version)` and the verbs `add|install|upgrade|check|list|remove`. `add`: normalize → derive/validate binding → hardened clone to a temp checkout → resolve commit → `read_manifest` → `resolve_transitive` → materialize self + deduped transitive into `<root>/imports/<binding>/` (refuse a transitive `agents/` dir, umbrella §7.2) → append `[imports.<n>]` to `camp.toml` → write lock entries (self + transitive `via`) → `inventory_executable` each materialized dir, self AND transitive (report + the `exec_inventory` field of `import.added`) → collect agent §5.4 refusals → append `import.added` (aggregated distinct ignored keys + skills/commands/nested-pack reports + `exec_inventory`) + one `import.refused` per agent-key refusal → print unbound-binding warnings for the pack's route values + the `--name` remedy (umbrella §7.1) + nested-pack report (umbrella §7.3). **Idempotent** for the same `(name, source, subpath, version)`; a different source for the same name → error. `install` never re-resolves a ref; `upgrade` is the only verb that moves a commit; `check` is offline; `remove` drops the entry + lock line + `<root>/imports/<n>/`.

- [ ] **Step 1: Failing test**

```rust
#[test]
fn add_from_file_repo_clones_locks_materializes() {
    let repo = tempfile::tempdir().unwrap();
    testsupport::init_repo(repo.path(), &[
        ("bmad/pack.toml", "[pack]\nname=\"bmad\"\nschema=2\n[imports.gc]\nsource=\"../gascity\"\n"),
        // a §5.4 key (pre_start) so the refusal→ledger path is exercised end-to-end:
        ("bmad/agents/architect/agent.toml", "scope=\"rig\"\nfallback=true\npre_start=\"boot\"\n"),
        ("bmad/agents/architect/prompt.template.md", "You are the architect."),
        ("bmad/skills/bmad-create-architecture/SKILL.md", "# skill"),
        // the TRANSITIVE parent carries executable content (§14.10): a check.path
        // reached only through bmad's [imports.gc] must still be inventoried.
        ("gascity/formulas/build-base.formula.toml",
         "formula=\"build-base\"\n[[steps]]\nid=\"s\"\ntitle=\"t\"\n[steps.check]\nmode=\"exec\"\npath=\"scripts/parent-verify.sh\"\n"),
    ]);
    let camp = tempfile::tempdir().unwrap();
    std::fs::write(camp.path().join("camp.toml"), "[camp]\nname=\"t\"\n[agent_defaults]\ntools=[\"Read\",\"Skill\"]\n").unwrap();
    camp_core::ledger::Ledger::open(&camp.path().join("camp.db")).unwrap();
    let url = format!("file://{}//bmad", repo.path().display());
    run_add(camp.path(), &url, Some("bmad"), None).unwrap();

    let cfg = camp_core::config::CampConfig::load(&camp.path().join("camp.toml")).unwrap();
    assert!(cfg.imports.contains_key("bmad"));
    assert!(!cfg.imports["bmad"].trust_exec, "an import is untrusted unless the operator opts in");
    let lock = camp_core::import::lock::PacksLock::read(&camp.path().join("packs.lock")).unwrap();
    assert!(lock.entry("bmad").is_some());
    let gc = lock.imports.iter().find(|e| e.subpath.as_deref()==Some("gascity")).unwrap();
    assert_eq!(gc.via.as_deref(), Some("bmad"));
    assert_eq!(camp_core::pack::resolve_agent(&cfg, "bmad.architect").unwrap().name, "bmad.architect");

    let led = camp_core::ledger::Ledger::open(&camp.path().join("camp.db")).unwrap();
    // import.added carries the aggregated exec inventory — INCLUDING the
    // check.path that arrived only through the transitive gascity parent
    // (§14.10; dies against Task 16's exec-vs-shell mutation):
    let added = led.events_of_type(camp_core::event::EventType::ImportAdded).unwrap();
    assert!(!added.is_empty());
    let inventory = added[0].data["exec_inventory"].to_string();
    assert!(inventory.contains("parent-verify.sh"),
        "transitive check.path must be inventoried: {inventory}");
    // §5.4 refusal appended as a ledger event, naming pack/agent/key:
    let refused = led.events_of_type(camp_core::event::EventType::ImportRefused).unwrap();
    assert!(refused.iter().any(|e|
        e.data["key"] == "pre_start" && e.data["agent"] == "architect" && e.data["binding"] == "bmad"),
        "one import.refused per refused key: {refused:?}");
}
#[test]
fn re_adding_same_source_is_idempotent_and_different_source_errors() {
    let repo = tempfile::tempdir().unwrap();
    testsupport::init_repo(repo.path(), &[
        ("bmad/pack.toml", "[pack]\nname=\"bmad\"\nschema=2\n"),
        ("bmad/agents/a/prompt.md", "a"),
    ]);
    let camp = tempfile::tempdir().unwrap();
    std::fs::write(camp.path().join("camp.toml"), "[camp]\nname=\"t\"\n[agent_defaults]\ntools=[\"Read\"]\n").unwrap();
    camp_core::ledger::Ledger::open(&camp.path().join("camp.db")).unwrap();
    let url = format!("file://{}//bmad", repo.path().display());
    run_add(camp.path(), &url, Some("bmad"), None).unwrap();
    run_add(camp.path(), &url, Some("bmad"), None).unwrap(); // idempotent
    let repo2 = tempfile::tempdir().unwrap();
    testsupport::init_repo(repo2.path(), &[("bmad/pack.toml", "[pack]\nname=\"bmad\"\nschema=2\n"), ("bmad/agents/a/prompt.md","a")]);
    let other = format!("file://{}//bmad", repo2.path().display());
    assert!(run_add(camp.path(), &other, Some("bmad"), None).is_err(), "same name, different source");
}
```

- [ ] **Step 2: Run — expect FAIL.**
- [ ] **Step 3: Implement** `run_add` and the verbs, plus `main.rs`: `Commands::Import { #[command(subcommand)] cmd }` with `Add { source, #[arg(long)] name, #[arg(long)] version }`, `Install`, `Upgrade { name: Option<String> }`, `Check`, `List`, `Remove { name }`; arms call `cmd::import::run_add`/`run_install`/etc. Binding derivation: `--name`, else the source's last subpath component, else the repo name; validate `[A-Za-z0-9_-]+`, non-empty, not `.`/`..`; on failure say to pass `--name`. Append `[imports.<n>]` by editing `camp.toml` text (mirror `rig::add`'s append style). Enforce the transitive-`agents/` refusal here (the materialized tree exists). Idempotency + different-source-error per the spec.
- [ ] **Step 4: Run — expect PASS.**
- [ ] **Step 5: Mutation check (two).** (a) Make `run_add` overwrite on a different source instead of erroring; `re_adding_same_source_is_idempotent_and_different_source_errors` fails. Revert. (b) Flip Task 16's `mode == "exec"` filter to `"shell"`; `add_from_file_repo_clones_locks_materializes` fails on the transitive `parent-verify.sh` inventory assertion (§14.10). Revert.
- [ ] **Step 6: Commit.**

```bash
git add crates/camp/src/cmd/import.rs crates/camp/src/main.rs
git commit -m "compat: camp import add|install|upgrade|check|list|remove with reporting + ledger events"
```

---

## Task 18: `camp order enable|disable` + `ls`

**Files:**
- Modify: `crates/camp/src/cmd/order.rs`
- Modify: `crates/camp/src/main.rs` (SHARED): `OrderCommand::Enable { name }`, `Disable { name }`
- Test: `cmd/order.rs` tests

**Interfaces:**
- Consumes: `CampConfig.orders_section.enabled` (Task 1), `compile_all_orders` (Task 14).
- Produces: `enable_order(camp_root, name)` / `disable_order(camp_root, name)` maintain `[orders] enabled`; `ls` gains source + disabled columns (component §9).

- [ ] **Step 1: Failing test**

```rust
#[test]
fn enable_adds_and_disable_removes_the_name() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("camp.toml"), "[camp]\nname=\"t\"\n[imports.bmad]\nsource=\"file:///x\"\n").unwrap();
    enable_order(dir.path(), "bmad.nightly").unwrap();
    let cfg = camp_core::config::CampConfig::load(&dir.path().join("camp.toml")).unwrap();
    assert!(cfg.orders_section.enabled.contains(&"bmad.nightly".to_string()));
    disable_order(dir.path(), "bmad.nightly").unwrap();
    let cfg = camp_core::config::CampConfig::load(&dir.path().join("camp.toml")).unwrap();
    assert!(!cfg.orders_section.enabled.contains(&"bmad.nightly".to_string()));
}
```

- [ ] **Step 2: Run — expect FAIL.**
- [ ] **Step 3: Implement.** `enable_order`/`disable_order`: load `CampConfig`, mutate `orders_section.enabled` (dedupe on enable), then rewrite the `[orders]` block in `camp.toml` (surgical: replace the existing `[orders]` block or append one if absent; keep the rest of the file intact — follow `rig::add`'s text-edit convention). Wire `ls` to `compile_all_orders` (print source + disabled columns). `main.rs` arms `Enable`/`Disable`.
- [ ] **Step 4: Run — expect PASS.**
- [ ] **Step 5: Mutation check.** Make `disable_order` a no-op; the disable assertion fails. Revert.
- [ ] **Step 6: Commit.**

```bash
git add crates/camp/src/cmd/order.rs crates/camp/src/main.rs
git commit -m "compat: camp order enable|disable maintains [orders] enabled; ls shows source + disabled"
```

---

## Task 19: `camp init` starter-pack flow (§8) + container

**Files:**
- Modify: `crates/camp/src/cmd/init.rs` (a pure `decide_import()` + the flow)
- Modify: `crates/camp/src/main.rs` (SHARED): `init` gains `--import <source>` / `--no-import`
- Modify: `contrib/docker/entrypoint.sh`, `contrib/docker/compose.yaml`
- Test: `cmd/init.rs` tests (the pure decision; never fetches the default source)

**Interfaces:**
- Produces:
  ```rust
  pub enum ImportDecision { Prompt, Install(String), Skip, HandOff }
  pub fn decide_import(is_tty: bool, import: Option<&str>, no_import: bool) -> ImportDecision;
  ```
  Table (component §8): TTY + no flag → `Prompt` (`Install the starter pack? [Y/n]`, default yes, names NO roles); `--import <src>` → `Install(src)` (no prompt, composes with `--exists-ok`); `--no-import` → `Skip`; NOT a TTY + no flag → `HandOff` (loud stderr, exact command). TTY = `stdin.is_terminal()`. Prompted-yes / `--import` fetch failure → exit non-zero ("camp WAS created, pack was NOT installed"). The default starter source is the pinned constant; NEVER fetched in a test.

- [ ] **Step 1: Failing test**

```rust
#[test]
fn decide_import_covers_the_matrix() {
    assert!(matches!(decide_import(true, None, false), ImportDecision::Prompt));
    assert!(matches!(decide_import(true, Some("file:///x"), false), ImportDecision::Install(s) if s=="file:///x"));
    assert!(matches!(decide_import(true, None, true), ImportDecision::Skip));
    assert!(matches!(decide_import(false, None, false), ImportDecision::HandOff));
    assert!(matches!(decide_import(false, Some("file:///x"), false), ImportDecision::Install(_)));
}
```

- [ ] **Step 2: Run — expect FAIL.**
- [ ] **Step 3: Implement `decide_import`** and wire the flow into `run()`: after the camp exists, `decide_import(std::io::stdin().is_terminal(), import.as_deref(), no_import)` → `Prompt` reads y/n; `Install(src)` calls `cmd::import::run_add(&root, &src, None, None)` and on failure `bail!` the created-not-installed message; `Skip` no-op; `HandOff` loud stderr. Add `const DEFAULT_STARTER_SOURCE` (component decision 12): `https://github.com/Liquescent-Development/gascamp/tree/main/packs/starter`, `version = "sha:<pinned>"` — pin to the gascamp commit carrying the rewritten starter (Task 20); note the sha in a comment + the PR. `main.rs`: add `--import` (`conflicts_with = "no_import"`; allowed WITH `--exists-ok`) and `--no-import`. `contrib/docker/entrypoint.sh`: pass `--import "$CAMP_PACK"` when set and run `camp import install` before `exec campd`.
- [ ] **Step 4: Run — expect PASS.**
- [ ] **Step 5: Mutation check.** Make not-a-TTY-no-flag return `Prompt`; `decide_import_covers_the_matrix` fails. Revert.
- [ ] **Step 6: Commit.**

```bash
git add crates/camp/src/cmd/init.rs crates/camp/src/main.rs contrib/docker/entrypoint.sh contrib/docker/compose.yaml
git commit -m "compat: camp init offers the starter pack (§8); --import/--no-import; container installs"
```

---

## Task 20: Rewrite `packs/starter/` as a Gas City directory pack

**Files:**
- Rewrite: `packs/starter/` — `agents/*.md` → `agents/<name>/` dirs; add `pack.toml`; `orders.toml` → `orders/<name>.toml`; keep the symlinked formula; rewrite `README.md`.
- Test: `crates/camp-core/tests/starter_pack.rs` (loads the real starter pack)

- [ ] **Step 1: Failing test** (`crates/camp-core/tests/starter_pack.rs`)

```rust
#[test]
fn starter_pack_is_a_valid_directory_pack() {
    let starter = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packs/starter");
    let m = camp_core::import::manifest::read_manifest(&starter).unwrap();
    assert_eq!(m.pack.name, "starter");
    assert!(starter.join("agents/dev/prompt.md").exists() || starter.join("agents/dev/prompt.template.md").exists());
    let (agent, refusals) = camp_core::pack::parse_agent_dir(&starter.join("agents/dev")).unwrap();
    assert_eq!(agent.name, "dev");
    assert!(refusals.is_empty());
    assert!(starter.join("orders").is_dir());
    assert!(starter.join("formulas/guarded-change.toml").exists());
}
```

- [ ] **Step 2: Run — expect FAIL** (still `.md` files, no `pack.toml`).
- [ ] **Step 3: Implement.**
  - Add `packs/starter/pack.toml`: `[pack]\nname = "starter"\nschema = 2\ndescription = "Starter pack — copy and adapt"\n`.
  - Convert each `agents/<name>.md` → `agents/<name>/prompt.md` (the body) + `agents/<name>/agent.toml` (keep `description`, and `scope` if present; DROP `model`/`tools`/`permissionMode` — now `[agent_defaults]`). Do this for `dev`, `reviewer`, `committer`; delete the `.md` files.
  - `orders.toml` → `orders/morning-triage.toml` + `orders/ci-red.toml` in gc's `[order]` shape (`formula`, `trigger`, `schedule`/`on`); delete `orders.toml`.
  - Keep `formulas/guarded-change.toml` as the existing relative symlink (the deref case).
  - Rewrite `README.md`: replace `packs = ["packs/starter"]` with `camp import add packs/starter --name starter`; route with `--agent starter.dev`; note model/tools come from `[agent_defaults]`.
- [ ] **Step 4: Run — expect PASS.** `cargo test -p camp-core --test starter_pack 2>&1 | tail`
- [ ] **Step 5: Mutation check.** Rename `agents/dev/prompt.md` → `notes.md`; the test fails. Restore.
- [ ] **Step 6: Commit.**

```bash
git add packs/starter
git commit -m "compat: starter pack is a Gas City directory pack (pack.toml, agent dirs, orders/ dir)"
```

---

## Task 21: #80 — a fresh camp knows zero agents until the starter import

**Files:**
- Test: `crates/camp/src/cmd/init.rs` tests (or `crates/camp/tests/init_starter.rs`)

**Interfaces:** Consumes Tasks 12, 17, 20. The failing-then-passing test the kickoff requires for #80. (The "failing" state is the git history: before Tasks 12/17/20 exist, the assertions cannot compile/pass; the branch's commit chain IS the failing→passing record. The test itself captures the whole arc in one run: zero agents → import → resolve.)

- [ ] **Step 1: Write the test**

```rust
#[test]
fn fresh_camp_has_no_agents_until_starter_import() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".camp");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("camp.toml"), "[camp]\nname=\"t\"\n[agent_defaults]\ntools=[\"Read\",\"Skill\"]\n").unwrap();
    camp_core::ledger::Ledger::open(&root.join("camp.db")).unwrap();
    let cfg = camp_core::config::CampConfig::load(&root.join("camp.toml")).unwrap();
    // #80: zero agents — the route fails (the documented dead-end):
    assert!(camp_core::pack::resolve_agent(&cfg, "starter.dev").unwrap_err().to_string().contains("starter"));
    // the fix: import the LOCAL starter pack (a file source; never the network):
    let starter = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packs/starter");
    crate::cmd::import::run_add(&root, &starter.to_string_lossy(), Some("starter"), None).unwrap();
    let cfg = camp_core::config::CampConfig::load(&root.join("camp.toml")).unwrap();
    assert_eq!(camp_core::pack::resolve_agent(&cfg, "starter.dev").unwrap().name, "starter.dev");
}
```

- [ ] **Step 2: Run — expect PASS** (all machinery is in place by now). Run: `cargo test -p camp fresh_camp_has_no_agents 2>&1 | tail`.
- [ ] **Step 3:** No new impl. If it fails, fix the responsible earlier task — do not patch around it.
- [ ] **Step 4: Run — expect PASS.**
- [ ] **Step 5: Mutation check.** Point the import at a non-pack dir; `run_add` errors (no `pack.toml`) — confirms the pack gate. Revert.
- [ ] **Step 6: Commit.**

```bash
git add crates/camp/src/cmd/init.rs
git commit -m "compat: #80 — fresh camp resolves its first agent only after a pack import"
```

---

## Task 22: #85 — export round-trips a gc-discoverable directory pack

**Files:**
- Modify: `crates/camp-core/src/export.rs` (verify `copy_tree` copies agent DIRECTORIES; count agents as immediate subdirectories)
- Test: `export.rs` tests

**Interfaces:** Consumes Tasks 10/20. #85 disappears by construction (umbrella §5.1): camp's native agent IS gc's directory, so export copies `agents/` verbatim and gc discovers each subdirectory. The failing→passing arc is the format retirement (Task 10/20): before this branch, `<camp>/agents/` held `.md` FILES → gc discarded them; after, they are directories gc discovers.

- [ ] **Step 1: Write the test**

```rust
#[test]
fn exported_pack_is_gc_discoverable_directory_shaped() {
    let (dir, ledger) = temp_ledger();
    let camp_root = dir.path();
    let adir = camp_root.join("agents/dev");
    std::fs::create_dir_all(&adir).unwrap();
    std::fs::write(adir.join("agent.toml"), "description = \"dev\"\n").unwrap();
    std::fs::write(adir.join("prompt.md"), "do the work").unwrap();
    std::fs::write(camp_root.join("camp.toml"), "[camp]\nname=\"golden\"\n").unwrap();
    let cfg = crate::config::CampConfig::load(&camp_root.join("camp.toml")).unwrap();
    let out = tempfile::tempdir().unwrap();
    let report = export_city(&ledger, &cfg, camp_root, &out.path().join("city"),
        &ExportOptions { skip_untranslatable: false }).unwrap();
    let exported = out.path().join("city/pack/agents/dev");
    assert!(exported.is_dir(), "exported agent must be a DIRECTORY gc discovers");
    assert!(exported.join("prompt.md").exists());
    assert_eq!(report.agents, 1);
}
```

- [ ] **Step 2: Run — expect FAIL** if the count or dir-copy is off; else PASS.
- [ ] **Step 3: Implement** any needed fix in `export.rs::write_pack`/`copy_tree` so agent *directories* are copied and `report.agents` counts one per immediate `agents/` subdirectory (not per file). Keep the symlink refusal in `agents/`.
- [ ] **Step 4: Run — expect PASS.** Also `cargo test -p camp-core export:: 2>&1 | tail` (golden fixtures stay green; export never wrote a `packs` key — component §13).
- [ ] **Step 5: Mutation check.** Make `copy_tree` skip directories; the test fails. Revert.
- [ ] **Step 6: Commit.**

```bash
git add crates/camp-core/src/export.rs
git commit -m "compat: #85 — export copies agent directories verbatim (gc-discoverable by construction)"
```

---

## Task 23: `GCPACKS_REF` + the phase-1 corpus-load gate

**Files:**
- Create: `ci/gc-compat/GCPACKS_REF`
- Create: `ci/gc-compat/load_corpus_packs.py`
- Modify: the CI workflow that runs `ci/gc-compat/` (mirror the `GASCITY_REF` job; fetch at `GCPACKS_REF`; never vendor)

**Interfaces:** **Phase-1 scope:** the gate asserts pack/agent/transitive LOADING, not formula compilation (the §9 rung table is phase 2). Seeds the compat gate §10 calls for.

- [ ] **Step 1: Create `GCPACKS_REF`** (one line, `GASCITY_REF` mold): `44b2eef94f035283b70df62d3bd1fc77bce13d56`
- [ ] **Step 2: Write `ci/gc-compat/load_corpus_packs.py`**

```python
#!/usr/bin/env python3
# usage: python3 ci/gc-compat/load_corpus_packs.py <corpus-checkout>
# Phase-1 gate: pack/agent/transitive LOADING (not formula compilation — phase 2).
# Assert the numbers pinned at GCPACKS_REF. Exit non-zero on any drift.
import glob, os, sys, tomllib
root = sys.argv[1]
def die(m): print("GCPACKS gate FAIL:", m); sys.exit(1)
importers = {}
for pt in glob.glob(os.path.join(root, "*", "pack.toml")):
    with open(pt, "rb") as fh: d = tomllib.load(fh)
    if d.get("imports"): importers[os.path.basename(os.path.dirname(pt))] = d["imports"]
if set(importers) != {"bmad","gstack","compound-engineering","superpowers"}: die(f"importers {set(importers)}")
for p, imp in importers.items():
    if imp.get("gc",{}).get("source") != "../gascity": die(f"{p} gc import != ../gascity")
if glob.glob(os.path.join(root,"gascity","agents","*","")): die("gascity should have no agents/")
if not os.path.isfile(os.path.join(root,"gascity","roles","pack.toml")): die("gascity/roles nested pack missing")
def n(p): return len(glob.glob(os.path.join(root,p,"agents","*","")))
for p, expect in {"bmad":10,"gstack":13,"compound-engineering":28,"superpowers":9,"gascity/roles":12}.items():
    if n(p) != expect: die(f"{p} agents {n(p)} != {expect}")
ref = open(os.path.join(os.path.dirname(__file__),"GCPACKS_REF")).read().strip()
print("GCPACKS gate OK at", ref)
```

- [ ] **Step 3: Run locally against the clone.** `python3 ci/gc-compat/load_corpus_packs.py /tmp/gcpacks-compat1` → `GCPACKS gate OK at 44b2eef94f035283b70df62d3bd1fc77bce13d56`
- [ ] **Step 4: Wire CI.** Add/extend a job that checks out `gastownhall/gascity-packs` at `$(cat ci/gc-compat/GCPACKS_REF)` and runs the gate. Never commit the corpus tree (umbrella §10). Follow the existing `check_vocab.sh` / `GASCITY_REF` job shape.
- [ ] **Step 5: Mutation check.** Change an expected count (bmad 10→11) and re-run locally; the gate exits non-zero. Revert.
- [ ] **Step 6: Commit.**

```bash
git add ci/gc-compat/GCPACKS_REF ci/gc-compat/load_corpus_packs.py .github/workflows/*.yml
git commit -m "compat: pin GCPACKS_REF + phase-1 corpus-load gate (pack/agent/transitive)"
```

---

## Task 24: The §3 two-command recipe — end-to-end acceptance against `file://` fixtures

**Files:**
- Test: `crates/camp/tests/two_command_recipe.rs` (integration test; `file://` fixtures, no network, no `claude`)

**Interfaces:** Consumes everything. The umbrella §12 phase-1 sentence, end to end. (Duplicate a small `init_repo` helper in the test file, or expose `cmd::import::testsupport::init_repo` + `run_add` via `pub` for integration reach.)

- [ ] **Step 1: Write the acceptance test**

```rust
#[test]
fn two_command_recipe_materializes_bmad_transitive_gascity_and_roles_bound_gc() {
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path(), &[
        ("bmad/pack.toml", "[pack]\nname=\"bmad\"\nschema=2\n[imports.gc]\nsource=\"../gascity\"\n"),
        ("bmad/agents/architect/agent.toml", "scope=\"rig\"\nfallback=true\n"),
        ("bmad/agents/architect/prompt.template.md", "architect {{.Var}}"),
        ("bmad/skills/bmad-create-architecture/SKILL.md", "# skill"),
        ("gascity/formulas/build-base.formula.toml", "formula=\"build-base\"\n"),
        ("gascity/roles/pack.toml", "[pack]\nname=\"gc-roles\"\nschema=2\n"),
        ("gascity/roles/agents/run-operator/prompt.md", "operate"),
        ("gascity/roles/agents/review-synthesizer/prompt.md", "gc synth"),
        ("gstack/pack.toml", "[pack]\nname=\"gstack\"\nschema=2\n[imports.gc]\nsource=\"../gascity\"\n"),
        ("gstack/agents/review-synthesizer/prompt.md", "gstack synth"),
    ]);
    let camp = tempfile::tempdir().unwrap();
    let root = camp.path();
    std::fs::write(root.join("camp.toml"), "[camp]\nname=\"t\"\n[agent_defaults]\ntools=[\"Read\",\"Skill\"]\n").unwrap();
    camp_core::ledger::Ledger::open(&root.join("camp.db")).unwrap();
    let base = format!("file://{}", repo.path().display());

    // The two commands (§3), against LOCAL file:// (never the network):
    run_add(root, &format!("{base}//bmad"), Some("bmad"), None).unwrap();
    run_add(root, &format!("{base}//gascity/roles"), Some("gc"), None).unwrap();

    let cfg = camp_core::config::CampConfig::load(&root.join("camp.toml")).unwrap();
    assert_eq!(camp_core::pack::resolve_agent(&cfg, "bmad.architect").unwrap().name, "bmad.architect");
    let lock = camp_core::import::lock::PacksLock::read(&root.join("packs.lock")).unwrap();
    assert!(lock.imports.iter().any(|e| e.subpath.as_deref()==Some("gascity") && e.via.as_deref()==Some("bmad")),
        "transitive gascity materialized with via=bmad");
    assert_eq!(camp_core::pack::resolve_agent(&cfg, "gc.run-operator").unwrap().name, "gc.run-operator");
    assert!(camp_core::pack::resolve_formula(&cfg, "build-base").is_ok(), "gascity contributes formula layers");

    // add gstack too: the cross-binding collision coexists:
    run_add(root, &format!("{base}//gstack"), Some("gstack"), None).unwrap();
    let cfg = camp_core::config::CampConfig::load(&root.join("camp.toml")).unwrap();
    assert!(camp_core::pack::resolve_agent(&cfg, "gstack.review-synthesizer").unwrap().prompt.contains("gstack"));
    assert!(camp_core::pack::resolve_agent(&cfg, "gc.review-synthesizer").unwrap().prompt.contains("gc"));
    // an unbound binding fails naming the remedy:
    assert!(camp_core::pack::resolve_agent(&cfg, "superpowers.implementer").unwrap_err().to_string().contains("camp import add"));
}

#[test]
fn transitive_relative_source_escaping_the_repo_is_refused_at_add() {
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path(), &[
        ("bmad/pack.toml", "[pack]\nname=\"bmad\"\nschema=2\n[imports.gc]\nsource=\"../../etc\"\n"),
        ("bmad/agents/a/prompt.md", "a"),
    ]);
    let camp = tempfile::tempdir().unwrap();
    std::fs::write(camp.path().join("camp.toml"), "[camp]\nname=\"t\"\n[agent_defaults]\ntools=[\"Read\"]\n").unwrap();
    camp_core::ledger::Ledger::open(&camp.path().join("camp.db")).unwrap();
    let url = format!("file://{}//bmad", repo.path().display());
    let err = run_add(camp.path(), &url, Some("bmad"), None).unwrap_err().to_string();
    assert!(err.to_lowercase().contains("escape") || err.contains("repo"), "{err}");
}
```

- [ ] **Step 2: Run — expect FAIL** if any wiring is incomplete.
- [ ] **Step 3:** No new impl — fix the responsible earlier task on any failure (fail fast; do not special-case).
- [ ] **Step 4: Run — expect PASS.** Then the full gate: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace 2>&1 | tail -30`.
- [ ] **Step 5: Mutation check.** Break dedupe (Task 9); the `via=bmad` single-materialization assertion fails. Revert.
- [ ] **Step 6: Commit.**

```bash
git add crates/camp/tests/two_command_recipe.rs
git commit -m "compat: end-to-end §3 two-command recipe against file:// fixtures (acceptance)"
```

---

## Final verification (before the PR)

- [ ] Full gate to a clean result: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace`.
- [ ] Corpus gate locally: `python3 ci/gc-compat/load_corpus_packs.py /tmp/gcpacks-compat1` → `GCPACKS gate OK`.
- [ ] No sibling-owned file touched: `git diff --name-only main... | grep -E 'daemon/(spawn|patrol|dispatch|event_loop)\.rs'` must be EMPTY.
- [ ] Open ONE PR from `compat-1-import-binding`; foreground-watch `gh pr checks --watch` to the settled result; report PR number + settled CI status + each acceptance criterion quoted with its evidence (test name + command output), per the kickoff.

## Acceptance criteria (quote each in the PR, with evidence)

1. **"the §3 two-command recipe materializes a bmad-shaped pack + its transitive gascity + a roles pack bound as gc against LOCAL file:// fixture repos (never the network)"** → `two_command_recipe.rs::two_command_recipe_materializes_bmad_transitive_gascity_and_roles_bound_gc`.
2. **"#80 (fresh camp, zero agents) ... failing-then-passing test"** → `cmd::init::tests::fresh_camp_has_no_agents_until_starter_import` (Task 21) + the Task 1–20 commit chain.
3. **"#85 (export round-trip) ... failing-then-passing test"** → `export::tests::exported_pack_is_gc_discoverable_directory_shaped` (Task 22).
4. **"every §14 phase-1 obligation has a named test"** → the §14 obligation→test table (all rows).
5. **"CI green"** → `gh pr checks --watch` settled green + the corpus gate.

---

## Open coordination items for the lead (surfaced, not resolved here)

1. **Skills dispatch-install call-site (`spawn.rs`, sibling-owned).** Task 15 ships `install_skills` + tests; the dispatch-time call belongs in `crates/camp/src/daemon/spawn.rs` (fix-82/fix-86) and is a phase-3 integration (gc pack agents are dispatch-only; their worker env/shims are phase 3). Phase-1 acceptance is materialization, so this does not block phase 1 — but the lead should schedule the wiring when phase 3's worker-env lands. No spec ambiguity; a boundary artifact of the parallel split.
2. **`AgentDef`/`resolve_agent` signature stability.** The plan preserves both so `dispatch.rs`/`patrol.rs`/`sling.rs`/`spawn.rs` never need editing. If a sibling changes `AgentDef`'s fields, a rebase surfaces it — re-run the full gate after any sibling merge (kickoff rebase protocol).
3. **Existing tests that dispatch a tool-less agent** now hit §5.2's "refuse without a resolved allowlist." Any such test in a sibling-owned file must gain `[agent_defaults].tools` after rebase; flagged so the lead can sequence it. Within owned files, Tasks 10–12 update every affected test.
4. **`yaml_rust2` dependency — RESOLVED by plan-gate amendment (2026-07-13).** Task 10 drops the last `use` in owned `pack.rs` only; the manifest entry in the shared `Cargo.toml`/`Cargo.lock` is NOT removed in this stream (additive-only shared-file rule; an unused workspace dep does not fail `clippy -D warnings`). See Follow-ups.

## Follow-ups (post-merge, lead-sequenced)

- **yaml_rust2 removal from workspace Cargo.toml deferred** — operator/lead-sequenced chore after this stream merges.

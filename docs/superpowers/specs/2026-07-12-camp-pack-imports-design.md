# camp pack imports — design

**Date:** 2026-07-12
**Issues:** [#80](https://github.com/Liquescent-Development/gascamp/issues/80) (a fresh camp cannot run its first bead) · [#84](https://github.com/Liquescent-Development/gascamp/issues/84) (the gc parity gap)
**Status:** rev 2 — the **import-machinery component spec**. The umbrella design is
`2026-07-12-gas-city-pack-compatibility-design.md`, which this feeds; where the two
disagree, the umbrella wins. Two decisions here are **overridden** by it: the agent
format (the Claude Code `.md` file is retired in favour of Gas City agent directories),
and pack content (camp loads a real gc pack's agents, formulas, and orders, not only
camp-format ones).

## 1. The problem

Two problems, and the second one is the reason this spec is bigger than "add a verb".

**A fresh camp knows zero agents.** `camp init` writes `camp.toml` (`[camp] name`), `camp.db`, and a gitignore entry. Nothing else. So:

```
$ camp sling "anything"
camp: no agent to route to: pass --agent <name>, set default_agent on [[rigs]] "rig", ...
$ camp sling --agent dev "anything"
camp: unknown agent "dev"; searched [".../c/agents"]
```

Both refusals are correct and fail fast. The gap is that there is no supported way past them. `packs/starter/` exists only in the source tree; `make install` installs binaries and nothing else. The workaround in the wild is to point `camp.toml` at whatever clone happens to exist — e.g. `~/.claude/plugins/marketplaces/gascamp/packs/starter` — a plugin manager's implementation detail, machine-absolute, and not something that may be load-bearing.

**Pack layering was only half-built, and the code says so.** Spec §11 defines a pack as `agents/ + formulas/ + orders.toml (+ skills/, commands/)`. Only `agents/` was ever wired in:

- `pack.rs:175` — `layers.push(dir.join("agents"))` is the **only** pack subdirectory pushed anywhere in the codebase.
- `orders/mod.rs:264` — `formula_path()` resolves `<camp>/formulas/<name>.toml` and carries the promise *"Phase 12's pack layering replaces this body"*. It never landed.
- Orders are `[[order]]` tables in `camp.toml` only (`orders/parse.rs:57` reads `config.orders`).
- Therefore **`packs/starter/orders.toml` is read by no code path**, and `packs/starter/formulas/guarded-change.toml` is reachable only by an operator who types its path.

So an import system that only layered agents would faithfully materialize a pack's formulas and orders into a directory nothing reads — a silent no-op, on day one, with our own starter pack. This spec finishes the layering rather than cementing the hole.

## 2. Provenance: what Gas City actually does

Camp is "what k3s is to k8s" for Gas City (README), so this surface is modelled on `gc import`, read at the ref this repo already pins (`ci/gc-compat/GASCITY_REF` = `12410301…`).

- **Verbs** (`cmd/gc/cmd_import.go:94-105`): `add`, `remove`, `check`, `install`, `upgrade`, `list`, `status`, `why`, `migrate`, `prune`. There is **no `credential` verb** (rev 1 claimed one; it does not exist).
- **`add`**: `gc import add <source> [--name <binding>] [--version <constraint>]` (`:112`, `:152-153`).
- **Sources are richer than rev 1 claimed.** Both of these are real, disambiguated by position:
  - a **leading** `//` on a local path means *city-root-relative* (`cmd_import.go:1366`);
  - an **embedded** `//` in a remote source **is** the go-getter subdir separator — `internal/config/pack_include.go:30-41` documents `<source>//<subpath>#<ref>`, e.g. `git@github.com:org/repo.git//topo#v1.0`.
  GitHub tree URLs (`https://github.com/{owner}/{repo}/tree/{ref}[/{path}]`, ref = branch **or** tag) are the *convenience* form, with a documented limitation: *"ref is parsed as a single path component. For branches with `/` in the name, use the `source//subpath#ref` format instead"* (`pack_include.go:87-88`). Transports accepted: `https`, `http`, `ssh`, `file` (`cmd_import.go:1335-1338`).
- **Lock** (`internal/packman/lockfile.go:22-33`): `schema` (int) + packs keyed by source, each `{version, commit, fetched}`. There is **no** `repository` field and **no** stored cache location — the cache path is *derived* from `RepoCacheKey(source, commit)` (`cache.go:32-44`).
- **Cache**: shared and machine-local (`cache.go:25-31`), not copied into the project.
- **Public pack constants** exist (`internal/config/public_packs.go:15`), and each is paired with a **`sha:`-pinned version** (`:20`) that pins fresh-init output to a release commit.
- **Untrusted-remote hardening** (`internal/git/git.go:385-395`, applied in `packman/cache.go:254-271`): `http.followRedirects=false`, `protocol.allow=never` plus an explicit allowlist of `https/http/ssh/git/file`, alongside `core.hooksPath=/dev/null`, `core.fsmonitor=false`, `core.untrackedCache=false` and a sanitized `GIT_*` environment.
- **Collisions**: gc's own design direction is *"collisions in the same public slot should be hard errors"* (`engdocs/design/pack-import-export-surface.md:316-320`), called a deliberate breaking change requiring migration diagnostics.

## 3. Decisions

1. **Adopt gc's model: declare → lock → install.** `[imports.<name>]` in `camp.toml`, a tracked `packs.lock`, `camp import install` to materialize.
2. **Materialize into the camp, not a shared machine cache.** Keeps spec §12's "a camp dir stands alone" and keeps every path camp-relative. Deliberate divergence from gc, logged in #84.
3. **The materialization directory is `.camp/imports/<name>/`, gitignored.** Not `.camp/packs/`: a hand-authored local pack is a thing people write, and ignoring `.camp/packs/` would silently swallow it. `.camp/imports/` is owned by `camp import`; nothing is authored there.
4. **Source grammar is gc's full set, normalized on `add`.** Accepted: a local path; a remote git URL (`https`/`http`/`ssh`/`file`, including `git@host:path`); a GitHub tree URL; and the generic `<repo-url>//<subpath>#<ref>`. The generic form is what makes slash-branches (`release/1.2`), non-GitHub hosts, and `file://` test fixtures representable — rev 1's grammar excluded `file://`, which its own test plan required.
5. **The ref lives in exactly one place.** `add` **normalizes**: it parses the source into `repository` + `subpath` + `ref`, then stores `source` in canonical repo form, `subpath`, and the ref as `version`. A ref may be given in the tree URL *or* `#ref` *or* `--version`, but the stored config has one. Supplying two that disagree is an error, not a precedence puzzle.
6. **`[imports.<name>]` is the only pack surface. `packs = [...]` is removed.** A local pack is an import whose source is a path (gc's own model). A relative path source resolves against the **camp root** — the rule `packs` used, so no existing path shifts meaning. `version` on a path source is rejected, not ignored.
7. **A pack is a directory with a `pack.toml`.** Camp adopts the manifest `camp export` **already writes** (`export.rs:597-606`: `[pack] name / schema / description`), so a camp pack is a valid gc pack and vice versa. gc *requires* `pack.toml` (`packman/install.go:481`); rev 1's claim that sources round-trip "in both directions" was false without it. A pack with no manifest is a hard error. `packs/starter/` gains one.
8. **Layer everything a pack defines — agents, formulas, and orders — finishing Phase 12.** §6 specifies each.
9. **A cross-import name collision is a hard error, for agents and formulas alike.** Two imports both defining `dev` fails loudly, naming both imports and the name.
   - *Why it must change:* `packs` was an ordered array, so "last wins" was well-defined. `[imports.<name>]` are TOML **tables**; agent resolution must not silently depend on table iteration order. Erroring removes the ordering dependency entirely.
   - It is strictly more fail-fast: today a second pack **silently shadows** the first (`pack.rs:215-229`, pinned by `resolve_agent_layers_packs_last_wins_with_local_agents_highest`, `pack.rs:376-422` — that test is deliberately inverted by this design). `load_layer` already hard-errors on duplicates *within* a layer; this is the same rule, consistently applied.
   - `<camp>/agents/` and `<camp>/formulas/` remain the **sanctioned override layer**, and stay highest. Shadowing is legal exactly where it is explicit.
10. **An imported pack's orders are inert until explicitly enabled.** This is a money invariant, not a preference — see §7.
11. **`camp init` offers the starter pack, and never prompts where it cannot.** See §8. The prompt names **no roles**: `Install the starter pack? [Y/n]`. (Rev 1's prompt read `(dev, reviewer, committer)` — three role names in a line of Rust, violating AGENTS.md's *"zero roles in code"* and master spec §11, two decisions after arguing no code path names an agent.)
12. **The default starter source is a constant, pinned to a commit.** gc pairs every public pack source with a `sha:`-pinned version so fresh-init output is reproducible; rev 1 defaulted to the mutable `main`. Camp does the same: source `https://github.com/Liquescent-Development/gascamp/tree/main/packs/starter`, `version = "sha:<pinned>"`, moved deliberately by a PR.
13. **Port gc's untrusted-remote git hardening.** See §11. Rev 1 declined it on a premise this design itself falsifies.
14. **Master spec §11's law survives.** The binary carries **zero role content** — only a default *source URL*, which names a pack, not a role, and no code path names an agent (decision 11 removed the one that did). The pack is fetched, never embedded.

## 4. Pack shape

```
<pack>/
  pack.toml          [pack] name / schema = 2 / description      (required)
  agents/*.md        Claude Code agent definitions               → layered
  formulas/*.toml    Gas City formula-v2 subset                  → layered
  orders.toml        [[order]] tables                            → layered, INERT (§7)
  skills/ commands/  Claude Code content                         → IGNORED by camp
```

`skills/` and `commands/` are for Claude Code to install as a plugin; camp has no use for them. That is a design decision, not an oversight — so `camp import add` **reports what it ignored**. Dead content is never silent.

## 5. Config surface

`camp.toml` (tracked — the source of truth):

```toml
[camp]
name = "myproj"

[imports.starter]                                    # git-backed: locked + materialized
source = "https://github.com/Liquescent-Development/gascamp"
subpath = "packs/starter"
version = "sha:7ff0980be0f4f3f1c1f2e4b8b7a6d5c4e3f2a1b0"

[imports.house]                                      # local path: layered in place
source = "../packs/house"                            # no fetch, no lock entry

[orders]
enabled = ["starter.nightly-review"]                 # the ONLY thing that arms a pack order (§7)
```

`.camp/packs.lock` (tracked — reproducibility):

```toml
schema = 1                                           # gc versions its lock; a tracked artifact with
                                                     # no version stamp is where "no backcompat" bites
[[import]]
name = "starter"
source = "https://github.com/Liquescent-Development/gascamp"
subpath = "packs/starter"
version = "sha:7ff0980…"
commit = "7ff0980be0f4f3f1c1f2e4b8b7a6d5c4e3f2a1b0"  # what was actually resolved
fetched = "2026-07-12T19:36:55Z"
```

**`location` is derived, never stored** — it is always `.camp/imports/<name>/`. Rev 1 stored it, which made a path read from a tracked file into a write-anywhere primitive (`location = "../../../../etc/…"` in a pulled lock).

**Binding names are validated**: `[A-Za-z0-9_-]+`, non-empty, and never `.` or `..` — they become directory names. A name is derived from the source's last subpath component (else the repo name) when `--name` is absent; if that is not a legal name, or already bound, `add` fails and says to pass `--name`.

Layout:

```
.camp/
  camp.toml           tracked
  packs.lock          tracked
  imports/            GITIGNORED — owned by `camp import`
    starter/{pack.toml,agents/,formulas/,orders.toml}
  agents/  formulas/  the sanctioned override layers, highest
  camp.db  worktrees/ already gitignored
```

`gitignore::RUNTIME_DIRS` gains `imports`. Verified safe: every generated pattern is a leading-slash-anchored literal, `.camp/` is never ignored wholesale by camp's generator, and camp writes to the **repo-root** `.gitignore` — so `.camp/packs.lock` stays tracked exactly as `.camp/camp.toml` does.

## 6. Resolution

**Agents** — layers, lowest to highest: each import's `agents/`, then `<camp>/agents/`. Order within the import group is irrelevant *by construction*: a name defined by two imports is an error (decision 9), so no import can shadow another. Only `<camp>/agents/` may shadow, and it does so explicitly.

**Formulas** — the same shape, replacing the dead `formula_path()`. `resolve_formula(cfg, name)` searches each import's `formulas/`, then `<camp>/formulas/` (highest). Cross-import collision is an error. Every caller that resolves a formula — `camp sling --formula`, and an order's `formula` field — goes through it, so a pack's formula is finally reachable.

**Orders** — an import's `orders.toml` is parsed and its orders are namespaced `<import>.<name>` (so two imports can never collide). Camp-local `[[order]]` tables in `camp.toml` keep their bare names and stay active, exactly as today: the operator wrote them.

**Symlinks must be dereferenced on materialization.** `packs/starter/formulas/guarded-change.toml` is a *relative symlink* into the gc-validated corpus (`../../../crates/camp-core/tests/fixtures/formulas/valid/`) — Phase 12 decision D3, one source of truth. Its target lives **outside the pack subpath**, so materializing `packs/starter` alone would produce a **dangling symlink** and formula layering would break on the very pack we ship. Materialization therefore **copies with symlinks dereferenced**, and fails fast on a dangling link or one whose target escapes the *repository* root.

## 7. The money invariant

An order fires a formula; a formula dispatches workers; workers cost real money. Therefore:

> **Nothing an import brings may ever fire until the operator names it in `[orders] enabled`.**

- An imported pack's orders load, validate, and appear in `camp order ls` as **disabled**, with their source.
- They are armed only by `[orders] enabled = ["<import>.<order>"]`.
- `camp order enable <name>` / `camp order disable <name>` write that list; `camp import add` prints what it found and the exact command to arm it.
- Camp-local `[[order]]` tables are unaffected — the operator authored them in their own `camp.toml`.

Without this, `camp import add <url>` could arm a cron from a pack you just downloaded and bill you on a schedule you never wrote. This invariant is the reason pack orders are worth having at all, and it must be pinned by a test that fails if an imported order ever fires unenabled.

## 8. `camp init`

| Situation | Behaviour |
|---|---|
| TTY, no flag | prompt `Install the starter pack? [Y/n]` (default yes) |
| `--import <source>` | install it, no prompt |
| `--no-import` | skip |
| **not a TTY**, no flag | never prompt: skip, and print a loud stderr hand-off naming the exact command |
| fetch fails (prompted-yes **or** `--import`) | **exit non-zero**: "the camp WAS created, the pack was NOT installed", mirroring the existing service-install wording |

TTY means **`stdin.is_terminal()`** — the stream the answer is read from. (`echo | camp init` from a terminal has a TTY on *stdout* but not stdin; an implementer who checks stdout hangs.)

**`--import` composes with `--exists-ok`.** Rev 1 made them mutually exclusive, which made it *structurally impossible for the container to ever get a pack*: `contrib/docker/entrypoint.sh` **must** pass `--exists-ok` or it crash-loops, and it is not a TTY — so the container would land in exactly the zero-agent state this design exists to kill, while decision 7 called `install` "the container verb". `--exists-ok --import <src>` now means **ensure the camp exists; ensure the import is declared and materialized** — idempotent, which is what an entrypoint needs. This is not the auto-repair §11 forbids: the operator asked, explicitly, with `--import`.

`--import` and `--no-import` remain mutually exclusive (clap `conflicts_with`).

`contrib/docker/` is updated in the same wave: the entrypoint passes `--import "$CAMP_PACK"` when set, and runs `camp import install` before `exec campd` so a persisted `camp.toml` + `packs.lock` with an unmaterialized `imports/` comes up working.

**Offline:** `camp init` is offline-safe only on the `--no-import` and not-a-TTY paths. A prompted **yes** that cannot reach the network **fails** — the camp exists, the pack does not, exit non-zero. No silent fallback (AGENTS.md), and the operator who is offline answers `n`.

## 9. Verbs

`camp import add <source> [--name <n>] [--version <ref|sha:…>]`
: normalize the source (§ decision 5), write `[imports.<n>]`, resolve the ref to a commit, write the lock entry, materialize, and report what it ignored (`skills/`, `commands/`) and what it found (orders — disabled, with the enable command).
: **Idempotent**: re-adding the same `(name, source, subpath, version)` is a no-op success, re-materializing if `.camp/imports/<n>/` is missing. The same name bound to a *different* source is an error — use `upgrade` or `remove`.

`camp import install`
: materialize `camp.toml` + `packs.lock` as they stand. The fresh-clone / CI / container verb. **Never re-resolves a ref**: the lock's commit is authoritative, or it is not a lock.

`camp import upgrade [name]`
: the **only** verb that moves a commit. Re-resolves the declared ref to its current commit, rewrites the lock, re-materializes. With no semver solving (#84), "upgrade" means exactly "re-resolve the ref I declared".

`camp import check`
: materialized content matches the lock's commit, and `camp.toml` matches the lock. **Offline by design** — it never consults the remote, so *a moved branch is not drift*. Only `upgrade` can see that. Exit non-zero on drift.

`camp import list` · `camp import remove <name>`
: list shows each import, its source, and the commit it is locked at. remove drops the entry, the lock line, and `.camp/imports/<name>/`.

`camp order enable <name>` · `camp order disable <name>`
: maintain `[orders] enabled` (§7). `camp order ls` gains a source column and a disabled state.

Not implemented (nothing in camp for them to operate on — #84): `why`, `list --tree`, `prune`, `status`, `migrate`.

## 10. Error handling

| Condition | Behaviour |
|---|---|
| `[imports.x]` declared, `.camp/imports/x/` absent | hard error: `import "x" is not installed — run \`camp import install\`` |
| `camp.toml` ↔ `packs.lock` disagree | `camp import check` fails, naming the drift |
| fetch/clone fails | fail fast: name the source and git's own stderr |
| two imports define one agent **or formula** name | hard error naming both imports and the name |
| pack has no `pack.toml` | hard error — it is not a pack |
| materialized symlink dangles or escapes the repo root | hard error (§6) |
| an imported order names a formula no layer provides | hard error at load, naming the order and the formula |
| pack ships `skills/`/`commands/` | reported as ignored — visible, never silent |

The first row kills the failure mode that made this whole episode hard to read: a camp that cannot see its agents currently resolves to **zero agents, silently**, and the operator discovers it via a confusing `patrol.degraded` about an "unknown agent". (The *stale-config* half of that confusion is a separate defect — #81 — and is not addressed here.)

## 11. Security

`camp import` introduces camp's **first production git subprocess** — today every `Command::new("git")` outside tests is absent, and git is invoked only by test helpers. It runs `git clone` on a URL that, because `camp.toml` and `packs.lock` are **tracked**, arrives from a `git pull`, a PR branch, or CI — **not** from the operator's hands. Rev 1 declined gc's hardening on the premise that the URL is always operator-typed; this design's own decision 1 falsifies that premise.

Every network git invocation therefore carries gc's flags verbatim (`internal/git/git.go:385-395`, `packman/cache.go:254-271`):

```
-c http.followRedirects=false
-c protocol.allow=never
-c protocol.https.allow=always   -c protocol.http.allow=always
-c protocol.ssh.allow=always     -c protocol.git.allow=always
-c protocol.file.allow=always
-c core.hooksPath=/dev/null      -c core.fsmonitor=false
-c core.untrackedCache=false
```

plus a sanitized environment with `GIT_*` variables stripped. `protocol.allow=never` + an explicit allowlist is what blocks the `ext::` transport (arbitrary command execution); `core.hooksPath=/dev/null` is what stops a cloned repo's hooks from running (the CVE-2022-24765 class). These are **arbitrary-code-execution fences, not merely SSRF fences** — rev 1 mischaracterized them.

A single `git()` helper owns these flags so there is one enforcement path, and a test asserts the argv byte-for-byte — gc pins its argv the same way, for the same reason.

## 12. Testing

- **No network, ever.** Git-backed imports run against local `file://` repos built in a temp dir (`git init`, commit a pack, clone from it) — the real clone/lock/materialize path, not a mock. This is why the source grammar must accept `file://` (decision 4).
- **No API spend, ever.** No test dispatches a real `claude`; workers are `#!/bin/sh` fakes.
- **The money invariant gets a test that can fail** (§7): an imported pack whose `orders.toml` contains a due cron order must fire **nothing** until `[orders] enabled` names it. Mutate the gate and this test must go red.
- **The hardening argv is asserted byte-for-byte** (§11). A flag silently dropped is a fence silently removed.
- **The symlink dereference is tested against the real starter pack** (§6): materialize it, then assert `formulas/guarded-change.toml` is a regular file with the corpus's content — the case that would otherwise dangle.
- Source normalization (tree URL, `//subpath#ref`, ssh, file, local path → repository + subpath + ref) is a pure function with unit tests, including conflicting-ref and malformed cases.
- `camp init`'s decision (prompt / `--import` / `--no-import` / not-a-TTY) is a pure decision function, tested as `service::decide` already is. The default starter source is **never fetched in a test**.
- Every new test must die against a mutation of the code it guards.

## 13. Migration and blast radius

**Removing `packs` is a hard parse error, by design and on purpose.** `CampConfig` is `#[serde(deny_unknown_fields)]` (`config.rs:14`), so an existing `camp.toml` with `packs = [...]` will **fail to load** — including on **campd's hot-reload path** (`daemon/orders.rs:230`), where a *running daemon will refuse the reloaded config*. This is sanctioned (no backwards compatibility is assumed) but it must be loud and self-explaining: config load detects the `packs` key specifically and errors with the rewrite, rather than emitting serde's bare "unknown field".

```toml
# before                      # after
packs = ["packs/starter"]     [imports.starter]
                              source = "packs/starter"
```

A bonus, worth stating because it argues *for* the change: the bare top-level `packs = [...]` had to precede every `[section]` header or TOML bound it to the last `[[rigs]]` table and `deny_unknown_fields` threw a confusing error (`README.md:237-252`). `[imports.<name>]` deletes that entire class of bug.

Touched in the same wave: `packs/starter/` gains `pack.toml`; `contrib/docker/entrypoint.sh` and `compose.yaml`; `pack.rs` (layers, collisions); `orders/mod.rs` (`formula_path` → `resolve_formula`); `orders/parse.rs` (pack orders, namespacing, the enabled gate); `config.rs` (`packs` → `imports`, `[orders] enabled`); `gitignore.rs` (`imports`); the inverted test at `pack.rs:376-422`. `camp export` is **unaffected**: it never reads `config.packs` and never writes a `packs` key — verified against the golden fixtures.

## 14. Out of scope

Tracked in #84: the transitive import graph, `[[exports]]`/namespaces, semver constraint solving, registry catalog handles, a shared machine-local cache, credentials for private pack repos, and `why`/`--tree`/`prune`/`status`/`migrate`.

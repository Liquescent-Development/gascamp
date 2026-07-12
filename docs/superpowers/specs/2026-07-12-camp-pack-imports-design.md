# camp pack imports — design

**Date:** 2026-07-12
**Issue:** [#80](https://github.com/Liquescent-Development/gascamp/issues/80)
**Follow-up gap:** [#84](https://github.com/Liquescent-Development/gascamp/issues/84)
**Status:** approved (decisions 1–10 taken 2026-07-12)

## 1. The problem

A fresh `camp init` writes `camp.toml` (`[camp] name`), `camp.db`, and a
gitignore entry. Nothing else. No agents, no packs, no rigs. The camp knows
**zero agents**, so it cannot run its first bead:

```
$ camp sling "anything"
camp: no agent to route to: pass --agent <name>, set default_agent on [[rigs]] "rig",
      or set default_agent under [dispatch] in .../camp.toml

$ camp sling --agent dev "anything"
camp: unknown agent "dev"; searched [".../c/agents"] (packs in camp.toml order, then <camp>/agents/)
```

Both refusals are correct and fail fast. The gap is that there is no supported
way past them. `packs/starter/` (`dev`, `reviewer`, `committer`) exists only in
the source tree; `make install` installs the binaries and nothing else. In
practice this gets worked around by pointing `camp.toml` at whatever clone
happens to exist — e.g. the Claude Code marketplace checkout at
`~/.claude/plugins/marketplaces/gascamp/packs/starter`. That path is a plugin
manager's implementation detail, it is machine-absolute, and it must not be
load-bearing.

**Camp needs a way to get a pack, and a fresh camp needs to arrive with one.**

## 2. Provenance: what Gas City actually does

Camp is "what k3s is to k8s" for Gas City (README), so the import surface is
modelled on `gc import` rather than invented. Read at the ref this repo already
pins for CI (`ci/gc-compat/GASCITY_REF` = `1241030…`):

- `gc import add <source> [--name <binding>] [--version <constraint>]`, plus
  `install`, `check`, `list [--tree]`, `upgrade`, `remove`, `why`, `status`,
  `prune`, `credential` (`cmd/gc/cmd_import.go`).
- Sources: local paths; local paths inside a git worktree (promoted to a
  `file://` repo source + subpath, locked to the commit); remote git repos;
  and **remote subpaths as dereferenceable tree URLs**,
  `https://github.com/{owner}/{repo}/tree/{ref}[/{path}]` — ref may be a branch
  or a tag (`internal/config/pack_include.go`).
- `packs.lock` pins the resolved graph: repository, version, commit, cache
  location (`internal/packman/`).
- gc ships **built-in public pack source constants**
  (`internal/config/public_packs.go`), e.g.
  `https://github.com/gastownhall/gascity-packs/tree/main/gascity`.

Two corrections to prior assumptions, recorded because they were both believed
and both wrong:

- `//` in a gc source is **not** a repo-subdir separator (the go-getter form).
  It means *path relative to the city root* (`cmd_import.go:1395`). Remote
  subpaths use tree URLs.
- gc does **not** copy packs into the project. It declares, locks, and
  materializes into a cache.

## 3. Decisions

1. **Adopt gc's model: declare → lock → install.** `[imports.<name>]` in
   `camp.toml`, a tracked `packs.lock`, and `camp import install` to
   materialize. Not a vendored copy-in-place.
2. **Materialize into the camp, not a shared machine cache.** gc caches by
   source+commit under a shared root; camp materializes into the camp itself.
   This keeps spec §12's "a camp dir stands alone", keeps every path in
   `camp.toml` and `packs.lock` camp-relative, and reuses the gitignore
   machinery that already exists. Cost: two camps importing one pack fetch it
   twice — irrelevant at camp's scale. Divergence logged in #84.
3. **The materialization directory is `.camp/imports/<name>/`, gitignored.**
   Deliberately *not* `.camp/packs/`: a hand-authored local pack is a thing
   people will write, and gitignoring `.camp/packs/` would silently ignore it.
   `.camp/imports/` is owned by `camp import` — nothing is authored there.
4. **Source syntax is gc's, verbatim.** Local paths, and
   `https://github.com/{owner}/{repo}/tree/{ref}[/{path}]`. A source string is
   portable between a camp and a city, in both directions. `--version` accepts
   a tag/branch ref or `sha:<commit>`; semver *constraint* solving is out of
   scope (#84).
5. **`[imports.<name>]` is the only pack surface. `packs = [...]` is removed.**
   gc has no `packs` array — a local pack is an import whose source is a path.
   One surface, one mental model. A path source is layered **in place**: no
   fetch, no lock entry, no materialization (this is gc's "local paths outside
   git worktrees: stored as plain paths, with no lock entry"). A **relative**
   path source resolves against the **camp root** — the same rule `packs = [...]`
   used, so the meaning of an existing path does not shift. Absolute paths are
   permitted. `version` is meaningless on a path source and is rejected, not
   ignored.
6. **A cross-pack agent-name collision is a hard error.** Two imports both
   defining `dev` fails loudly, naming both imports and the name.
   - *Why it must change:* `packs` was an ordered array, so "last wins" was
     well-defined. `[imports.<name>]` are TOML **tables**, and agent resolution
     must not silently depend on table iteration order. Erroring removes the
     ordering dependency entirely.
   - It is also strictly more fail-fast: today a second pack silently shadows
     the first. `pack::load_layer` already hard-errors on duplicate names
     *within* one layer; this is the same rule, consistently applied.
   - `<camp>/agents/` remains the **one sanctioned override**, and stays
     highest. Shadowing is legal exactly where it is explicit.
7. **Verbs:** `camp import add | install | list | remove | upgrade | check`.
   Dropped: `why`, `list --tree`, `prune`, `credential`, `status` — every one
   of them inspects the transitive import graph or the shared cache, and camp
   has neither (#84). Precisely:
   - `add` — resolve the source, write `[imports.<name>]`, resolve the ref to a
     commit, write the lock entry, materialize.
   - `install` — materialize `camp.toml` + `packs.lock` as they stand. The
     fresh-clone / CI / container verb. Never re-resolves a ref: the lock's
     commit is authoritative, or it is not a lock.
   - `upgrade [name]` — **the only verb that moves a commit.** Re-resolves the
     declared ref (branch or tag) to its current commit, rewrites the lock,
     re-materializes. With no semver constraints (#84), "upgrade" means exactly
     "re-resolve the ref I declared".
   - `check` — the materialized content matches the lock's commit, and
     `camp.toml` matches the lock. Exit non-zero on any drift.
   - `remove <name>` — drop the entry, the lock line, and `.camp/imports/<name>/`.
   - `list` — each import, its source, and the commit it is locked at.
8. **`camp init` offers the starter pack, and never prompts where it cannot.**
   - Interactive TTY, no flag → prompt `Install the starter pack (dev,
     reviewer, committer)? [Y/n]`, default yes.
   - `--import <source>` → install it, no prompt.
   - `--no-import` → skip.
   - **Not a TTY** (Docker entrypoint, CI) → never prompt: skip, and print a
     visible stderr hand-off naming the exact command. This is the shape
     `camp init` already uses for `Decision::SkipNoManager` — an absent
     capability is a loud hand-off, never a silent fallback.
   - `--import` and `--no-import` are mutually exclusive at the clap layer
     (`conflicts_with`), exactly as `--service` / `--no-service` already are.
   - `--exists-ok` returns **before** the import decision, as it already returns
     before the service decision, and for the same reason: `--exists-ok` is a
     no-op on an existing camp, never a repair. `--import --exists-ok` is
     therefore rejected by clap, so the short-circuit can never swallow an
     explicit request to install a pack.
   - An explicit `--import` that fails to fetch **exits non-zero**, with the
     "the camp WAS created, the pack was NOT installed" context, mirroring the
     existing service-install wording in `cmd/init.rs`.
9. **The default starter source is a constant**, exactly as gc does it:
   `https://github.com/Liquescent-Development/gascamp/tree/main/packs/starter`.
10. **Spec §11's law survives intact.** §11 says the plugin "ships **no agent
    definitions** … if the machinery mentions a role, it is a bug", and that the
    starter pack is content "to copy, not a dependency". Fetching the pack from
    the public repo means the `camp` binary carries **zero role content** — only
    a default *source URL*, which names a pack, not a role. No code path names
    an agent. gc sets the same precedent with `PublicGascityPackSource`.

## 4. Config surface

`camp.toml` (tracked — the source of truth):

```toml
[camp]
name = "myproj"

[imports.starter]                                     # git-backed: locked + materialized
source = "https://github.com/Liquescent-Development/gascamp/tree/main/packs/starter"
version = "main"                                      # branch, tag, or sha:<commit>

[imports.house]                                       # local path: layered in place
source = "../packs/house"                             # no fetch, no lock entry
```

`.camp/packs.lock` (tracked — reproducibility):

```toml
[[import]]
name = "starter"
source = "https://github.com/Liquescent-Development/gascamp/tree/main/packs/starter"
repository = "https://github.com/Liquescent-Development/gascamp"
subpath = "packs/starter"
version = "main"
commit = "7ff0980be0f4f3f1c1f2e4b8b7a6d5c4e3f2a1b0"   # what was actually resolved
location = "imports/starter"                          # camp-relative, never absolute
```

Layout:

```
.camp/
  camp.toml           tracked
  packs.lock          tracked
  imports/            gitignored — owned by `camp import`
    starter/
      agents/dev.md
  agents/             the sanctioned override layer, highest
  camp.db  worktrees/ ...   already gitignored
```

`gitignore::RUNTIME_DIRS` gains `imports`. `packs.lock` is **not** ignored: it
is a lock file, and a fresh clone plus `camp import install` must reproduce the
same bytes.

## 5. Resolution

The layer list becomes, in order:

1. each `[imports.<name>]`, resolved to `.camp/imports/<name>/agents/` (git
   source) or `<source>/agents/` (path source);
2. `<camp>/agents/` — highest, the sanctioned override.

Within layer group (1), **order is irrelevant by construction**: a name defined
by two imports is an error, so no import can shadow another. Only (2) may
shadow, and it does so explicitly.

## 6. Error handling

Every one of these is loud, and every one names the remedy:

| Condition | Behaviour |
|---|---|
| `[imports.x]` declared, `.camp/imports/x/` absent | hard error: `import "x" is not installed — run \`camp import install\`` |
| `camp.toml` and `packs.lock` disagree | `camp import check` fails, naming the drift |
| fetch/clone fails | fail fast: name the source and git's own stderr |
| two imports define one agent name | hard error naming both imports and the name (decision 6) |
| path-source directory missing | the existing `pack::layers` error, unchanged |

The first row kills the failure mode that made this whole episode hard to read:
a camp that cannot see its agents currently resolves to **zero agents,
silently**, and the operator finds out via a confusing `patrol.degraded` about
an "unknown agent". Under this design, an import that is declared but not
installed is an error naming the command to run. (The *stale-config* half of
that confusion is a separate defect — #81 — and is not addressed here.)

## 7. Security

`camp import add` runs `git clone` on an operator-supplied URL. gc hardens every
network git invocation against redirect-based SSRF and transport abuse because a
pack source can be attacker-influenced on its **API** import path
(`internal/packman/cache.go`). Camp has no API surface: `camp import add` is a
local dev command, run by hand, with a URL the operator typed. The threat model
is therefore weaker and the hardening is **not** replicated here.

This is a standing assumption, not a permanent one: **if camp ever accepts a
pack source from anywhere but the operator's own hands, this section is wrong
and the gc hardening must be ported** (#84).

## 8. Testing

- **No network, ever, in tests.** Git-backed imports are exercised against local
  `file://` repos built in a temp dir (`git init`, commit a pack, clone from
  it). This is the real clone/lock/materialize path, not a mock.
- **No API spend, ever.** No test dispatches a real `claude` worker; workers are
  `#!/bin/sh` fakes, per the standing rule.
- Tree-URL parsing (`{owner}/{repo}/tree/{ref}[/{path}]` → repository + subpath
  + ref) is a pure function with unit tests, including the malformed cases.
- The default starter source is **never fetched in a test**. `camp init`'s
  decision (prompt / `--import` / `--no-import` / not-a-TTY) is tested as a pure
  decision function, exactly as `service::decide` already is.
- Every new test must die against a mutation of the code it guards. A test that
  cannot fail is not a test.

## 9. Out of scope

Tracked in #84: the transitive import graph, `[[exports]]`/namespaces, semver
constraint resolution, registry catalog handles, a shared machine-local cache,
credentials for private pack repos, and `why`/`--tree`/`prune`/`status`.

## 10. Migration

`packs = [...]` is removed rather than deprecated (no backwards compatibility is
assumed unless asked for). The rewrite is one line per camp:

```toml
# before
packs = ["packs/starter"]

# after
[imports.starter]
source = "packs/starter"
```

`camp doctor` should name the old key explicitly if it sees it, rather than
letting an unknown key parse to "zero packs" — silently resolving to no agents
is the failure mode this whole design exists to kill.

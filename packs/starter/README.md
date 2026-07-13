# Starter pack — example content, copy don't depend

A **pack is a Gas City directory pack** (compat §5.1/§7): a `pack.toml`, an
`agents/` of agent **directories** (`agent.toml` + a prompt file), a
`formulas/` of formula-v2 files, and an `orders/` of order files. This
starter pack is an example to copy and adapt — **not** a dependency of camp.
The camp plugin ships zero roles; roles live in packs like this one.

```
packs/starter/
  pack.toml                     # [pack] name + schema (required)
  agents/dev/                   # identity = directory name; prompt + agent.toml
    prompt.md
    agent.toml
  agents/reviewer/
    prompt.md
    agent.toml
  agents/committer/
    prompt.md
    agent.toml
  formulas/guarded-change.toml  # a formula (Gas City formula-v2 subset)
  orders/
    morning-triage.toml         # cron order (gc [order] shape)
    ci-red.toml                 # event order (camp-only — labeled)
```

## Use it

Import the pack as a binding — model/permission/tools come from your
`[agent_defaults]`, never the pack (compat §5.2):

```sh
camp import add packs/starter --name starter
```

A local path is a first-class source (no clone, no ref). Agents resolve as
`starter.dev`, `starter.reviewer`, `starter.committer`. Route work to one
with `camp sling "title" --agent starter.dev`, or set a default:

```toml
[dispatch]
default_agent = "starter.dev"

[agent_defaults]
model = "sonnet"
tools = ["Read", "Edit", "Write", "Bash", "Grep", "Glob"]
```

The pack's `orders/*.toml` are INERT until you arm them
(`camp order enable starter.morning-triage`) — the money invariant
(compat §14).

## Notes

- `formulas/guarded-change.toml` is a symlink into camp's gc-validated corpus
  (`crates/camp-core/tests/fixtures/formulas/valid/`), so it is guaranteed to
  compile under the real Gas City `gc` compiler (spec §8.2 subset invariant) —
  one source of truth, no drift. The symlink is dereferenced on materialize.
- A powered-off or sleeping machine fires no orders until wake (spec §9). A
  supervised campd (`camp init`, or `camp service install` on an existing
  camp) fires armed orders from login onward — no `camp` command needed
  first.
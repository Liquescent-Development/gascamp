# Starter pack — example content, copy don't depend

A **pack is a directory** of Claude Code content (spec §11): agent
definitions, formulas, orders, and optional skills/commands. This starter
pack is an example to copy and adapt — **not** a dependency of camp. The camp
plugin ships zero roles; roles live in packs like this one.

```
packs/starter/
  agents/dev.md            # a Claude Code agent definition (role)
  agents/reviewer.md       # another role — review-only tool set
  agents/committer.md      # owns git — turns verified worktree work into a commit on the bead branch
  formulas/guarded-change.toml   # a formula (Gas City formula-v2 subset)
  orders.toml              # example scheduled / event-triggered orders
```

## Use it

Import the pack from your `camp.toml`:

```toml
packs = ["packs/starter"]
```

Resolution is last-wins with your local definitions highest (spec §11). Route
work to a role with `camp sling "title" --agent dev`, or set a default:

```toml
[dispatch]
default_agent = "dev"
```

## Notes

- `formulas/guarded-change.toml` is a symlink into camp's gc-validated corpus
  (`crates/camp-core/tests/fixtures/formulas/valid/`), so it is guaranteed to
  compile under the real Gas City `gc` compiler (spec §8.2 subset invariant) —
  one source of truth, no drift.
- `orders.toml` is an example; a powered-off or logged-out machine fires no
  orders until wake (spec §9). Install the launchd agent for fire-at-login.

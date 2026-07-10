# camp — the Gas Camp plugin (machinery only, zero roles)

This plugin makes a Claude Code session the control plane for a camp. It is
**machinery only** — it ships no agent definitions. Roles are pack content
(see `packs/starter/`). "If the machinery mentions a role, it is a bug"
(spec §11).

Everything here is a thin wrapper over the `camp` CLI, so the session's
scripting surface is identical to the terminal's (spec §13 guarantee 6).
`camp` must be on your `PATH`.

## Versioning

Bump `version` in `.claude-plugin/plugin.json` whenever plugin content
(commands, skills, hooks, statusline) changes. Installed copies are cached by
version, so an unchanged version can leave a stale plugin in place even after
`/plugin marketplace update` — bumping the version makes `/plugin` refresh
reliably.

## Slash commands

| Command | Wraps | Does |
|---|---|---|
| `/sling` | `camp sling` | Create work — a Tier-0 bead or a `--formula` run. Enqueue only; campd is the sole dispatcher (spec §8.4). |
| `/nudge` | `camp nudge` | Converse with any session — live over campd's held stdin, else via `claude --resume`. |
| `/status` | `camp top` | Fleet snapshot: live sessions, ready/open beads. |
| `/adopt` | `camp adopt` | Reconcile the session registry against reality. |
| `/events` | `camp events` | Print the event log — the whole story. |

## Hooks (fire-and-forget, always exit 0)

- **SessionStart** → `camp session register --hook-stdin` (idempotent) +
  `camp adopt`. Registers this attended session as `attended/<session_id>`.
- **SessionEnd** → `camp session end --hook-stdin --if-registered`. Marks the
  session stopped.

**Stop and SubagentStop are deliberately NOT wired.** `Stop` fires once per
*turn* (not at session end); `SubagentStop`'s `session_id` is the *parent*
session, so ending it would kill the attended session (spec §10 forbids
campd crashing/killing a session in the user's TUI). Attended teammates are
visible in the agent panel and record their own ledger events; campd-spawned
workers get their lifecycle from campd. (See the Phase 12 plan's D5.)

An optional **PostToolUse breadcrumb** (`hooks/post-tool-use.sh`) ships
unregistered — patrol watches transcripts instead (spec §10). Enable it only
if you want explicit per-tool breadcrumbs; it is time-window throttled.

*Registry caveats (attended rows are best-effort):*
- A resumed session whose row already ended is not re-registered (session
  names are unique forever) — a harmless no-op with a stderr note.
- `camp adopt` keeps live attended sessions and never crashes them (spec §10:
  campd must not crash/kill a session in the user's TUI).
- **Phantom-live rows:** if the TUI dies without SessionEnd firing (kill -9,
  crash, power loss), its row stays `live` in `camp top` / `/status`
  indefinitely — campd cannot observe an unattributable interactive process.
  Expected; a bounded reaper is a deferred follow-up.

## Statusline (opt-in)

`statusline/statusline.sh` renders `▲live ●ready ✖red` from a read-only
socket query — it never auto-starts campd and degrades to empty output plus a
stderr note when campd is down. It is the **main session** status line. A
plugin cannot auto-set the main `statusLine`, so wire it into your own
`~/.claude/settings.json` (opt-in per D6):

```json
{ "statusLine": { "type": "command",
                  "command": "\"${CLAUDE_PLUGIN_ROOT}\"/statusline/statusline.sh" } }
```

Note: Claude Code also has a distinct `subagentStatusLine` settings key (which
a plugin *can* ship). It is a **different** feature — it receives a `tasks`
array and renders one row body *per teammate* in the agent panel, not a single
fleet-wide badge. This plugin does not wire the fleet badge there, because the
schema and semantics differ; a purpose-built per-teammate row would be a
separate script.

## Skills

`skills/worker/SKILL.md` is the lifecycle contract a campd-spawned pack
worker follows: recall → claim → work → emit milestones → remember → close →
exit.

`skills/operator/SKILL.md` is its mirror for the human's own control-plane
session: campd is the sole dispatcher, the `camp/<bead>` branch is the
deliverable, and the operator reads camp output and reports a concise summary
(and awaits with `camp show --wait`) rather than thrashing raw output.

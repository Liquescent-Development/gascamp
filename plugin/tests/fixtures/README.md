# Plugin hook fixtures — recorded Claude Code hook stdin payloads

Each `*.json` is a representative hook stdin payload (schema confirmed
against code.claude.com/docs, retrieved 2026-07-08). The Rust integration
harness `crates/camp/tests/plugin_hooks.rs` feeds these to the plugin's
shell hooks and asserts the expectations below. Every hook is
fire-and-forget: it always exits 0 and never blocks the session.

| Fixture | Hook script | Wired? | Expected effect | Throttle |
|---|---|---|---|---|
| `session-start.json` (`source: startup`, `session_id: S-1`) | `session-start.sh` | SessionStart | appends ONE `session.woke` for `attended/S-1` (idempotent on repeat) + runs `camp adopt`; exit 0 | idempotent (dedup by registry existence) |
| `session-end.json` (`source: prompt_input_exit`) | `session-end.sh` | SessionEnd | appends ONE `session.stopped` for `attended/S-1` if live; `--if-registered` no-op otherwise; exit 0 | n/a |
| `post-tool-use.json` (`tool_name: Bash`) | `post-tool-use.sh` | **OFF by default** (not registered) | appends a `worker.milestone` breadcrumb; exit 0 | time-window `throttle breadcrumb` (`CAMP_BREADCRUMB_THROTTLE`, default 5s; `0` disables) |
| `statusline.json` | `statusline/statusline.sh` | statusline (opt-in) | resolves cwd, prints `camp top --statusline` badge or degrades to empty + stderr note | n/a |

`Stop` and `SubagentStop` are intentionally NOT wired: `Stop` fires per turn
(not at session end), and `SubagentStop`'s `session_id` is the *parent*
session — ending it would kill the attended session (spec §10). Only the
attended top-level session (SessionStart → SessionEnd) and campd-spawned
workers (campd birth/SIGCHLD) get lifecycle events. See the plan's D5.

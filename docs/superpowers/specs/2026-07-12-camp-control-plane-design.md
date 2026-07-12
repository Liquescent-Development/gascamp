# camp control plane — watching and steering agents

**Date:** 2026-07-12
**Status:** proposed
**Related:** `2026-07-12-gas-city-pack-compatibility-design.md` (§8.1 is a stub; this replaces it)

## 1. Why

Camp dispatches workers and you cannot see them. There is no way to watch an agent work, no way to interrupt one that has gone wrong, and no way to answer a tool-permission question it is blocked on. The ledger tells you what *happened*; nothing tells you what is *happening*.

Gas City solves this with tmux: agents are driven through a PTY, and `gc session attach mayor` drops you into the terminal. herdr solves it by owning the PTY and then **regex-matching the rendered TUI** to guess what the agent is doing.

**Camp needs neither, and can do better than both** — because camp never gave up the structured channel. Its workers already speak a typed event protocol. It does not need to scrape a screen to know what an agent is doing; it already knows. What is missing is not information. It is a *way to look at it*, and a *way to talk back*.

### The shape this must not foreclose

The operator's view is a TUI today. It is a **web UI over an API** later, with each agent sandboxed in its own VM, attached to remotely. None of that is in scope here — but the design must not make it impossible, and that costs nothing if decided now and everything if retrofitted.

## 2. What the worker channel can actually do

Camp spawns workers as a direct child process (`spawn.rs:178-216`):

```
claude --output-format stream-json --input-format stream-json --session-id <uuid> \
       [--model X] [--permission-mode Y] [--allowedTools ...] [--append-system-prompt ...] -p
```

and holds the child's **stdin as a live pipe** for its lifetime.

That pipe carries a **bidirectional control protocol**. This is not in the public docs; it is established from the Agent SDK's own transport (`@anthropic-ai/claude-agent-sdk@0.3.207`, `package/sdk.mjs`), which spawns **this same CLI** with `--output-format stream-json --verbose --input-format stream-json` and then speaks:

| direction | message |
|---|---|
| parent → CLI | `{"type":"control_request","request_id":"…","request":{"subtype":"interrupt"}}` — the SDK's `interrupt()` |
| **CLI → parent** | `{"type":"control_request","request_id":"…","request":{"subtype":"can_use_tool", …}}` — **the worker asks its parent for a permission decision** |
| CLI → parent | same, `subtype: "request_user_dialog"` |
| parent → CLI | `{"type":"control_response","response":{"subtype":"success"\|"error","request_id":"…","response":…}}` |

Other subtypes present in the shipped CLI (v2.1.207) and the SDK: `set_model`, `set_permission_mode`, `control_cancel_request`, `mcp_message`, and an `initialize` handshake the parent sends first (carrying hooks, MCP servers, and the system prompt).

### 2.0 The permission flow must be OPTED INTO at spawn — it is not automatic

**`can_use_tool` does not fire unless camp asks for it.** The SDK, when given a `canUseTool` callback, passes:

```
--permission-prompt-tool stdio
```

That flag is what tells the worker *"ask your parent over stdio for permission decisions."* Without it the CLI decides from `--permission-mode` / `--allowedTools` and never asks — the worker simply proceeds or refuses on its own.

So camp's worker argv gains `--permission-prompt-tool stdio`, and camp must implement the **parent side**: receive `control_request{subtype:"can_use_tool"}`, and reply with a `control_response`.

The SDK also enforces that `canUseTool` and a named `--permission-prompt-tool <mcp-tool>` are **mutually exclusive** ("use one or the other") — camp uses the `stdio` form, and must not also configure a permission-prompt MCP tool.

Without this section, an implementer builds the whole `BLOCKED` state, wires the ledger append, renders it in the fleet view — and it never triggers, because nothing ever asked the worker to ask.

**Consequences:**

1. **Interrupt works without a PTY.** No SIGINT, no terminal.
2. **Permission decisions can be delegated to a human at runtime** — the worker blocks, camp routes the question to an operator, the answer goes back over the pipe. This is the interactive permission prompt, as a *protocol* rather than a terminal dialog.
3. **Model and permission mode can change mid-session.**
4. All of it is **JSON over a pipe camp already holds** — so it survives a network boundary. A worker in a VM on another host is the same protocol with a different transport. A PTY would have to be *forwarded*.

### 2.1 The risk, named and accepted

**This protocol is undocumented.** Camp depends on an internal interface between Claude Code and its own SDK. There is no compatibility promise, and a `claude` release can change it.

Accepted (operator decision, 2026-07-12), with these mitigations, which are requirements, not aspirations:

- **One module owns the wire format.** Nothing else in camp constructs or parses a control message.
- **The wire shapes are pinned by tests** against recorded fixtures, so a change is a red build, not a silent misbehaviour.
- **Failures are loud.** An unrecognized control message, or a control response that never arrives, is an evented, operator-visible fault — never a swallowed timeout. Camp already gates on the worker binary; a protocol break must surface there.
- **The named fallback is a PTY.** If the protocol is withdrawn, the design degrades to a terminal substrate. That is a worse product, and it is survivable.

### 2.2 Two flags, and a live bug (#86)

**`--verbose` is MANDATORY, not optional, and camp does not pass it.** The shipped CLI (2.1.205–2.1.207) rejects the argv outright:

```js
if (l.outputFormat === "stream-json" && !l.verbose) {
  process.stderr.write("Error: When using --print, --output-format=stream-json requires --verbose\n"); ru(1)
}
```

`verbose` resolves **flag → settings → false**. Camp's `HeldStream` argv (`spawn.rs:187-193`) omits it, and dispatch works today **only because the maintainer's `~/.claude/settings.json` sets `"verbose": true`** — a file camp does not own. On CI, in a container, or for any other developer, **every dispatched worker dies at exit 1.** Filed as **#86**; fixed as part of phase 1.

Rev 1 of this spec said *"camp's streaming works today without it, so it is not required"* — a conclusion drawn from a machine whose config silently supplied it. `docs/design/2026-07-06-assumption-findings.md` is contaminated by the same setting and must be re-validated with `verbose` unset.

**`--include-partial-messages`** — not `--verbose` — is what gates text deltas and partial messages (SDK: `if (includePartialMessages) push("--include-partial-messages")`). `camp attach`'s live view needs it; autonomous dispatch does not.

### 2.3 THE READ CHANNEL — campd does not currently receive anything from a worker

Rev 1 asserted campd "already holds the worker's stdout." **It does not.** The worker's stdout is redirected to a **file** (`spawn.rs:265`: `File::create(&spec.stdout_path)`), and campd's poll registry holds exactly five tokens — `LISTENER`, `CONFIG_WATCH`, `SIGCHLD`, `PATROL_WATCH`, `SIGTERM_SIG` (`event_loop.rs:29-46`). **No worker fd is ever registered.**

So today: campd holds the worker's **stdin (write) only**. Every `control_response`, and every `can_use_tool` the CLI emits, goes to **stdout** — which campd never reads. Without a read channel, `can_use_tool` is unreceivable, `interrupt` is unacknowledgeable, and `attach` has no source.

**The naive fix is a trap.** Piping stdout into campd would break **worker adoption**, which `spawn.rs:253-256` deliberately preserves: *"workers intentionally outlive a killed campd… EOF does not kill a stream worker."* stdin EOF is survivable; a broken stdout pipe is not — the worker takes SIGPIPE when campd dies.

**Decision: campd tails the worker's stdout file with a `notify` watcher, over a self-pipe.**

This is not new machinery. **Patrol already does exactly this** — `patrol.rs:1-2`: *"transcript watches (notify → self-pipe, the config-watch mold)"*, with the backend named as FSEvents/inotify (`patrol.rs:61-64`). Camp already tails per-worker files, watched and unwatched dynamically, wired into this same poll loop.

| | |
|---|---|
| campd → worker | the held **stdin pipe** (already exists; this is how `camp nudge` works) |
| worker → campd | **stdout file**, tailed via `notify` → self-pipe (patrol's proven mold) |

**Why this and not the alternatives:**
- **Adoption is preserved.** No pipe for campd's death to break.
- **Invariant 1 is preserved.** FSEvents/inotify are OS events, not polling — and the existing `make perf` idle-CPU gate already runs with patrol's transcript watches active. The risk is retired in-tree, not argued.
- **No relay process per worker** (which would have cost invariant 2).

**What it obliges:**
- **Partial lines.** A `notify` event can land mid-line. The reader buffers until a newline; a partial JSON line is never parsed.
- **The stream file is unbounded** (same class as issue #64). Phase 1 must bound it — cap, rotate, or truncate-on-close — and say which.
- **Latency is filesystem-event latency**, not pipe latency. Fine for a permission prompt (human-speed) and for an interrupt ack. Not suitable for anything needing sub-millisecond response, and nothing here does.

## 3. The data already exists

| what | where | status |
|---|---|---|
| worker's typed event stream | `<camp>/sessions/<session>.json` (stdout, stream-json) | already written |
| the session transcript | `transcript_path`, `.jsonl` | already written, **already recorded in the ledger** on `session.woke` |
| session registry | `sessions(name, agent, rig, claude_session_id, transcript_path, pid, status, bead, spawned_ts, ended_ts)` | already exists |
| the write channel | held stdin pipe | already held (this is how `camp nudge` works) |

Nothing here needs to be invented. It needs to be **exposed**.

## 4. The protocol is the product

**campd's socket is the control plane, and it is the only path to a worker.** Every client — the TUI, the overseer agent, a future web UI — goes through it. No client gets a private path (no tailing files, no signalling pids).

### 4.1 Verbs

| verb | meaning |
|---|---|
| `sessions.list` | every session: name, agent, rig, bead, state, last activity, and whether it is **blocked on a permission decision** |
| `session.subscribe` | live typed events for one session, from a cursor (so a late joiner gets history, then follows) |
| `session.send_turn` | inject a user turn (this is `camp nudge`, promoted to the protocol) |
| `session.interrupt` | stop the current turn |
| `session.permission_decision` | answer a `can_use_tool` request: allow / deny / allow-always, with the request id |
| `session.set_model`, `session.set_permission_mode` | change either mid-session |
| `fleet.subscribe` | the aggregate stream: session state transitions, stalls, permission requests, completions |

### 4.2 Three rules that keep the future open

1. **Sessions are addressed by name, never by pid or file path.** The day a worker lives in a VM on another host, clients must not notice. A protocol that hands out pids is a protocol that cannot cross a machine boundary.
2. **campd owns the truth; clients are stateless renderers.** A TUI that tails files directly works beautifully until the files are on another machine.
3. **The transport is swappable; the protocol is not.** A unix socket today. Anything else later. The verbs do not change.

*(A remote API, a web UI, and per-agent VM isolation are **out of scope**. These three rules are the entire cost of not foreclosing them.)*

### 4.3 Invariant 1 holds — via the notify watch, not a fiction

campd wakes on the worker's stdout **file-change event** (§2.3), fans the parsed events out to subscribers, and sleeps again. No timer, no tick. A subscriber with nothing happening costs zero wakeups, because nothing is written and no event fires.

This is the property herdr could not offer: its `events.wait` is a 100ms sleep loop over a ring buffer with **no fd to block on**. Camp has an fd — the self-pipe patrol already uses.

*(Rev 1 claimed campd "already holds the worker's stdout." It does not, and that error made this section false. §2.3 is the fix.)*

### 4.4 `subscribe` is a new connection MODE, not just a new verb

Camp's socket is **one-shot** today: *"Send one request, read one response line"* (`socket.rs:314-318`), and `event_loop.rs:321-352` deregisters the connection after responding. `respond()` even documents its assumption — *"Responses are a few bytes; a WouldBlock here means the client is not reading."*

A long-lived, server-push, many-message subscription violates every one of those premises. It requires, and the implementation must provide:

- **per-connection output buffering** and `Interest::WRITABLE` registration;
- **no `REQUEST_TIMEOUT`** on a subscription (a quiet stream is not a wedged daemon);
- **an explicit backpressure policy: a subscriber that cannot keep up is dropped, loudly, with an event.** It is never blocked on, and events are never silently discarded. campd's loop is single-threaded — a stalled subscriber that campd waits on is a daemon-wide wedge (the bug class of issue #55).

## 5. The operator's view

### 5.1 The fleet view — `camp watch`

The thing you leave open on a second monitor. One line per live session:

```
AGENT              BEAD          STATE        FOR    LAST
bmad/architect     campdemo-14   working      2m14s  Edit(src/lib.rs)
bmad/dev           campdemo-15   BLOCKED      0m31s  ? Bash(cargo publish)   ← needs you
gstack/reviewer    campdemo-12   working      6m02s  Read(README.md)
bmad/dev           campdemo-11   stalled     14m50s  (no output 12m)
```

`BLOCKED` is the state that matters and that no existing tool surfaces: a worker waiting on a `can_use_tool` decision. It is a **question addressed to you**, and it should be impossible to miss — the fleet view is where you notice.

### 5.2 The agent view — `camp attach <session>`

Drill into one worker. Its typed event stream, rendered live: tool calls with their inputs, tool results, assistant text, token usage. The transcript is the scrollback.

Because these are **events and not a terminal buffer**, the view can do things a tmux pane fundamentally cannot:

- **filter** — "show me only the Edits", "only the failures"
- **replay** — scrub back through a finished session; the transcript is durable
- **diff** — what did this agent change?

From here: send a turn, interrupt, answer a permission request. Detach freely; the worker neither knows nor cares.

### 5.3 The permission flow

The one genuinely interactive thing an agent does:

1. Worker hits a tool call that its allowlist does not cover.
2. Worker emits `can_use_tool` on the pipe; campd records it and marks the session **BLOCKED**.
3. It surfaces in `camp watch` (and, later, wherever else you happen to be).
4. You answer — allow once, allow always, or deny with a reason.
5. campd writes the `control_response`; the worker continues.

**The decision is appended to the ledger** — who allowed what, when, for which bead. An agent asking to run `cargo publish` is a thing you want a record of.

**If nobody answers, the worker stays blocked.** It does not time out into a default, and it does not proceed. A permission question that answers itself is not a permission question. (Confirmed against the CLI: its `sendRequest` parks on a promise with **no timer** — the blocked-forever property is the CLI's, not camp's invention.)

#### 5.3.1 `stdio` alone does not make the flow fire

`--permission-prompt-tool stdio` only routes decisions the CLI **would otherwise have asked about**. Its `createCanUseTool` short-circuits first: `behavior === "allow"` returns immediately, `behavior === "deny"` returns immediately, and **only an "ask" reaches the parent.**

So `--permission-mode bypassPermissions`, or an `--allowedTools` that already covers everything, **suppresses the flow entirely** — and `bypassPermissions` is exactly what camp's pinned worker config uses today (`assumption-findings.md`, F7).

**Camp refuses the incoherent combination.** Configuring `--permission-prompt-tool stdio` together with a permission mode that can never ask is a **fail-fast error at spawn**, not a feature that quietly never fires. (Invariant 5.)

#### 5.3.2 A blocked worker must not hold a dispatch slot

`dispatch.rs:426,464` gate on `children.len() >= max_workers`, default **10** (`config.rs:62-64`). A BLOCKED worker is a live child. Left unhandled, **ten unanswered permission prompts silently deadlock the entire camp** — no new work dispatches, ever, and nothing says why.

**Decision: a BLOCKED worker does not count against `max_workers`**, and campd emits a `permission.pending` event on entry. Crossing a `max_blocked` threshold raises a loud, operator-visible saturation fault. The *decision* must never auto-answer; the *slot* must never be its hostage.

#### 5.3.3 A blocked worker cannot survive a campd restart, and must not pretend to

campd holds the worker's stdin. On a campd restart, **that pipe is gone** — even a BLOCKED state durably recorded in the ledger cannot be answered, because there is no channel left to answer on. The worker (which deliberately *outlives* campd, `spawn.rs:255`) would sit blocked forever, holding a worktree.

The CLI does offer redelivery — `initialize`'s response carries `pending_permission_requests` — but that serves a parent that can re-`initialize` **on a live stdin pipe**, and an adopted worker has none.

**Decision: on adoption, campd kills any worker with an unanswered permission request, loudly and evented.** It is not silently orphaned, and it is not silently resumed. (`camp adopt` already reconciles the registry against reality; this is one more reality.)

**BLOCKED state lives in the ledger** (an event, not memory), so `camp watch` renders the same truth after a restart as before it — including the fact that the worker was killed.

### 5.4 The overseer

An agent that holds the same socket: it can list sessions, read their streams, send them turns, and interrupt them. Camp already has an operator skill; under this design it becomes a **client of the control plane rather than a special case**, which is the only reason it is possible at all.

That it needs no new machinery is the strongest argument that the protocol is factored correctly.

## 6. What this is not

- **Not a terminal multiplexer.** No PTY, no panes, no attach-to-a-shell. If you want to *be* the agent, run `claude` yourself.
- **Not keystroke-level.** You send *turns* and *decisions*, not keypresses. There is no TUI to press keys into — that is the trade for having typed events instead of pixels, and it is the right one for an orchestrator.
- **Not a poller.** See §4.3.

## 7. Phases

0. **Fix #86 and build the read channel.** Pass `--verbose` (dispatch is broken on every clean machine without it). Tail the worker's stdout file via `notify` → self-pipe (§2.3), bound it, and stand up the real-`claude` gate (§8). **Nothing else in this spec is buildable until campd can hear a worker.**
1. **Protocol + control module.** The socket verbs; the one module that owns the wire format, with pinned fixtures. `interrupt` and `send_turn` first — they are the smallest end-to-end slice through the whole stack, and `interrupt` is only verifiable once phase 0 lands (its `control_response` arrives on the read channel).
2. **`camp watch`.** The fleet view. Delivers most of the value on its own, and it is the cheapest thing to build.
3. **`can_use_tool` + the permission flow.** The highest-value single feature: it turns "unattended agent stalls forever on a permission it cannot get" into "the operator answers a question." Requires the ledger append and the `BLOCKED` state.
4. **`camp attach`.** The per-agent view: live stream, filter, replay, send-turn, interrupt.
5. **The overseer** as a first-class client.

## 8. Testing

**A fake worker validates camp's state machine. It can never validate the contract with a binary camp does not control.** #86 is the proof: camp's argv is rejected by the real CLI on any clean machine, every test is green, and no `#!/bin/sh` fake could ever have said so — because a fake ignores argv, ignores the protocol, and agrees with whatever camp does.

So the strategy is two-layered, and rev 1's *"this is better than testing against a real `claude`"* is deleted as false:

- **Fake workers for the state machine.** A `#!/bin/sh` worker can genuinely hold up its end — `while read -r line; do case … esac; done` on stdin, NDJSON on stdout — so it can emit a `can_use_tool` on demand, deterministically, and drive BLOCKED, the ledger append, and the fleet view. Cheap, hermetic, no API spend.
- **A real-`claude` compatibility gate for the contract.** Opt-in and local like `make service-e2e`, but **required before a release**: spawn ONE real worker and assert the whole handshake — argv accepted, `initialize` round-trip, a forced `can_use_tool`, a `control_response`, an `interrupt`. **Pin the tested `claude` version and fail loudly on an unpinned one**, exactly as `ci/gc-compat/GASCITY_REF` pins the gc compiler for invariant 6.

Without the second layer, §2.1's mitigations are theatre: fixtures pin what camp *sends and parses*, never what the CLI *accepts and emits*. A release that renames a subtype leaves every fixture green and every worker broken.
- **The blocked-forever property gets a test that can fail:** a worker that emits `can_use_tool` and receives no answer must remain blocked, must not time out, and must not proceed. Mutate that and the test must go red.
- **Invariant 1 gets a test:** a campd with N subscribed clients and no activity performs zero wakeups. The existing `make perf` idle-CPU gate covers it if the subscribe path is genuinely push.
- Every new test must die against a mutation of the code it guards.

## 9. Decisions that were open questions

Rev 1 parked these. Each is a prerequisite, not a curiosity, so each is decided here.

- **The `initialize` handshake.** *Optional* for a parent that only wants `interrupt` + `can_use_tool` — the `stdio` handler is wired from argv at startup, not gated on the handshake. **Camp sends it anyway**, because its response carries `pending_permission_requests`, and that is the only redelivery mechanism there is.
- **`request_user_dialog`.** The CLI genuinely sends it under `stdio`. Camp **answers it with a deterministic `control_response{subtype:"error"}`** ("interactive dialogs are not supported"). It is neither ignored (which §2.1 would raise as a protocol fault) nor left to hang a worker forever.
- **Event history bound.** The stream file is unbounded (issue #64's class). **Phase 1 bounds it**: a byte cap with rotation, and `session.subscribe`'s cursor is only valid within the retained window — a cursor older than the window is an explicit error, never a silently truncated stream.
- **Multiple deciders.** **First answer wins**, and the ledger records who answered. Losing deciders get an explicit "already decided by X" response rather than silence.

## 10. Open questions

*(None that block phase 1.)*

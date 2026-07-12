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

Other subtypes present in the shipped CLI (v2.1.207) and the SDK: `set_model`, `set_permission_mode`, `control_cancel_request`, `mcp_message`.

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

### 2.2 One thing to verify before building

The SDK passes **`--verbose`** alongside `stream-json`; camp does **not** (`spawn.rs:186-192`). Camp's streaming works today without it, so it is not required for basic output — but it may gate the finer-grained events (text deltas, partial messages) a live view wants. **Verify first, against a fake worker; do not assume in either direction.**

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

### 4.3 Invariant 1 holds

**campd does not poll to serve this.** It already reaps SIGCHLD, already watches the ledger, and already holds the worker's stdout. `session.subscribe` and `fleet.subscribe` are **push** — campd fans out events it is already receiving. A subscriber with nothing happening costs zero wakeups.

This is the property herdr could not offer: its `events.wait` is a 100ms sleep loop over a ring buffer with no fd to block on. Camp has an fd.

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

**If nobody answers, the worker stays blocked.** It does not time out into a default, and it does not proceed. A permission question that answers itself is not a permission question. (This is why `BLOCKED` must be loud: an unattended camp *will* park on one.)

### 5.4 The overseer

An agent that holds the same socket: it can list sessions, read their streams, send them turns, and interrupt them. Camp already has an operator skill; under this design it becomes a **client of the control plane rather than a special case**, which is the only reason it is possible at all.

That it needs no new machinery is the strongest argument that the protocol is factored correctly.

## 6. What this is not

- **Not a terminal multiplexer.** No PTY, no panes, no attach-to-a-shell. If you want to *be* the agent, run `claude` yourself.
- **Not keystroke-level.** You send *turns* and *decisions*, not keypresses. There is no TUI to press keys into — that is the trade for having typed events instead of pixels, and it is the right one for an orchestrator.
- **Not a poller.** See §4.3.

## 7. Phases

1. **Protocol + control module.** The socket verbs; the one module that owns the wire format, with pinned fixtures. `interrupt` and `send_turn` first — they are the smallest end-to-end slice through the whole stack.
2. **`camp watch`.** The fleet view. Delivers most of the value on its own, and it is the cheapest thing to build.
3. **`can_use_tool` + the permission flow.** The highest-value single feature: it turns "unattended agent stalls forever on a permission it cannot get" into "the operator answers a question." Requires the ledger append and the `BLOCKED` state.
4. **`camp attach`.** The per-agent view: live stream, filter, replay, send-turn, interrupt.
5. **The overseer** as a first-class client.

## 8. Testing

- **No API spend, ever.** Every test drives a **fake worker** — a `#!/bin/sh` script that emits recorded stream-json and control_requests on stdin/stdout. This is *better* than testing against a real `claude`: it can produce a `can_use_tool` on demand, deterministically.
- **The wire format is pinned by fixtures** (§2.1). If a `claude` upgrade changes a shape, a test goes red — that is the entire point of confining it to one module.
- **The blocked-forever property gets a test that can fail:** a worker that emits `can_use_tool` and receives no answer must remain blocked, must not time out, and must not proceed. Mutate that and the test must go red.
- **Invariant 1 gets a test:** a campd with N subscribed clients and no activity performs zero wakeups. The existing `make perf` idle-CPU gate covers it if the subscribe path is genuinely push.
- Every new test must die against a mutation of the code it guards.

## 9. Open questions

- **`--verbose`** (§2.2) — required for fine-grained events, or not? Verify against a fake worker first.
- **Event history bound.** `session.subscribe` from a cursor implies retention. The stream file is already on disk and already unbounded — is that acceptable, or does it need a cap? (Note issue #64: `output_bounded` is time-bounded but not byte-bounded. Same class of problem.)
- **Multiple deciders.** Two operators attached, one permission request. First answer wins, or explicit ownership? First-wins is simplest and probably right, but it should be *decided*, not defaulted.

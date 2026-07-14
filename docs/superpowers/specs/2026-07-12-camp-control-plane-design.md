# camp control plane — watching and steering agents

**Date:** 2026-07-12
**Status:** rev 3 — after adversarial review (findings in `2026-07-12-KNOWN-DEFECTS.md`). Rev 2's three blockers all lived in the read channel: it borrowed patrol's name for machinery patrol does not have (B1), let the stall ladder SIGKILL a correctly-waiting BLOCKED worker (B2), and mandated a rotation scheme that is unimplementable against a live writer (B3). The shape of the fix, everywhere: **correctness never depends on a delivered filesystem event, a blocked worker is not a stalled worker, and the live stream file is append-only.**
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

So camp's worker argv gains `--permission-prompt-tool stdio` **where the agent's resolved permission configuration can ask at all** (§5.3.1 — the flag is per-agent, not unconditional), and camp must implement the **parent side**: receive `control_request{subtype:"can_use_tool"}`, and reply with a `control_response`.

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

`verbose` resolves **flag → settings → false**. Camp's `HeldStream` argv (`spawn.rs:187-193`) omits it, and dispatch works today **only because the maintainer's `~/.claude/settings.json` sets `"verbose": true`** — a file camp does not own. On CI, in a container, or for any other developer, **every dispatched worker dies at exit 1.** Filed as **#86**; fixed as part of phase 0.

Rev 1 of this spec said *"camp's streaming works today without it, so it is not required"* — a conclusion drawn from a machine whose config silently supplied it. `docs/design/2026-07-06-assumption-findings.md` is contaminated by the same setting and must be re-validated with `verbose` unset.

**`--include-partial-messages`** — not `--verbose` — is what gates text deltas and partial messages (SDK: `if (includePartialMessages) push("--include-partial-messages")`). `camp attach`'s live view needs it; autonomous dispatch does not.

### 2.3 THE READ CHANNEL — campd does not currently receive anything from a worker

Rev 1 asserted campd "already holds the worker's stdout." **It does not.** The worker's stdout is redirected to a **file** (`spawn.rs:265`: `File::create(&spec.stdout_path)`), and campd's poll registry holds exactly five tokens — `LISTENER`, `CONFIG_WATCH`, `SIGCHLD`, `PATROL_WATCH`, `SIGTERM_SIG` (`event_loop.rs:29-46`). **No worker fd is ever registered.**

So today: campd holds the worker's **stdin (write) only**. Every `control_response`, and every `can_use_tool` the CLI emits, goes to **stdout** — which campd never reads. Without a read channel, `can_use_tool` is unreceivable, `interrupt` is unacknowledgeable, and `attach` has no source.

**The naive fix is a trap.** Piping stdout into campd would break **worker adoption**, which `spawn.rs:253-256` deliberately preserves: *"workers intentionally outlive a killed campd… EOF does not kill a stream worker."* stdin EOF is survivable; a broken stdout pipe is not — the worker takes SIGPIPE when campd dies.

**Decision: campd reads the worker's stdout file. The `notify` watcher is a wake-up call; it is never the correctness mechanism.**

| | |
|---|---|
| campd → worker | the held **stdin pipe** (already exists; this is how `camp nudge` works) |
| worker → campd | **stdout file**: per-session byte offset, read to EOF on wake; a `notify` watch (→ self-pipe, the config-watch mold) provides low-latency wakes |

**This is new machinery, and rev 2 pretended otherwise.** Rev 2 said *"patrol already does exactly this."* False: patrol **watches; it never tails.** It reads no file content and keeps no offset — its handler sets a touched-flag and writes one self-pipe byte, and `drain_touched` resets a stall timer (`patrol.rs:168-194`). Patrol tolerates dropped events **only because an armed stall timer catches them** (`patrol.rs:513-522`: *"a false stall costs one nudge"*). What the read channel adds that patrol has nowhere: per-file byte offsets, partial-line buffering, reopen-after-restart, and a delivery guarantee. Those are the hard part, and they get designed, not inherited.

**The correctness rule — correctness never depends on a delivered event:**

- campd keeps a **byte offset per tailed stream file** (durable: it doubles as the subscription cursor, §9). Reading means: open-or-reuse the fd, seek to the offset, read to EOF, buffer any trailing partial line (a `notify` event can land mid-line; a partial JSON line is never parsed), advance the offset past each complete line. **campd persists its own offset only *after* a line's ledger effect commits, and on restart adoption reconciliation (§5.3.4) runs before tailing resumes from the persisted offset.** That ordering closes both crash windows. A `can_use_tool` campd died before reading is re-found on restart and handed to §5.3.4's adoption rule — the named kill, bead re-hooked — rather than skipped to EOF and left to the slower generic stall ladder (it cannot be *answered*: the adopted worker's stdin pipe died with the old campd, which is §5.3.4's whole premise; the offset discipline buys the fast, greppable outcome, not a reprieve). And a re-read line whose `permission.pending` already committed belongs to an already-killed session — campd never appends a duplicate pending for a `request_id` the ledger already carries.
- **On EVERY campd wake — any poll token, not just the watch — campd drains every tailed stream file to EOF** before going back to sleep. A socket request, a SIGCHLD, a config change, a patrol touch, a stall timer: all of them pick up anything a lost event left behind. The watch only makes the common case fast.
- **`notify`'s documented loss mode is handled as a re-read-everything signal.** On inotify queue overflow, `notify` emits `EventKind::Other` with `Flag::Rescan` and an **empty `paths` vec**. Any handler that iterates `event.paths` discards it — rev 2's did. The rule: a Rescan, an empty-path event, or any unrecognized event kind ⇒ drain **all** tailed files. Per-path dispatch is an optimization applied only to well-formed events.
- **The delivery bound, stated honestly:** a dropped filesystem event delays reading until the next wake — and a worker that has gone quiet *because it is waiting for campd* (a pending `can_use_tool`) has an **armed stall timer**, so the worst case is one stall interval, after which the ladder's first act is to drain the channel and discover the pending request (§5.3.3). Late by minutes in the pathological case; **never lost, never blocked forever.** This is patrol's own safety argument, applied to the channel that actually carries content.

**The stream file is append-only for the life of its writer — never rotated, never truncated (this kills rev 2's §9).** The child owns the file offset: `File::create` passes the fd as the worker's stdout for its whole life, and there is no `O_APPEND`. **Rotate** and the worker keeps writing to the renamed inode while campd tails a fresh empty file — every later `can_use_tool` silently lost, permanently. **Truncate** and the child's offset does not reset — the next write lands at the old offset, leaving a sparse hole of NUL bytes where campd expects JSON lines. So: *"a byte cap with rotation"* is not implementable against a live worker, and the spec stops asking for it. Bounding is real but happens elsewhere:

- **At session end (reap):** the stream file is disposed or compressed per retention policy, like any other session artifact. This is where issue #64's class is actually solved.
- **Live ceiling:** a per-session `max_stream_bytes` (config, generous default). Breaching it is a **loud session failure** — the worker is killed, the event names the cap, the bead re-hooks (invariant 5: fail fast, never corrupt the channel and keep going).

**Latency is filesystem-event latency**, not pipe latency. Fine for a permission prompt (human-speed) and for an interrupt ack. Not suitable for anything needing sub-millisecond response, and nothing here does.

## 3. The data already exists

| what | where | status |
|---|---|---|
| worker's typed event stream | `<camp>/sessions/<session>.json` (stdout, stream-json) | already written |
| the session transcript | `transcript_path`, `.jsonl` | already written, **already recorded in the ledger** on `session.woke` |
| session registry | `sessions(name, agent, rig, claude_session_id, transcript_path, pid, status, bead, spawned_ts, ended_ts)` | already exists |
| the write channel | held stdin pipe | already held (this is how `camp nudge` works) |

Nothing here needs to be invented. It needs to be **exposed** — through the read channel §2.3 builds.

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

### 4.3 Invariant 1 holds — and the current perf gate does NOT yet prove it

campd wakes on the worker's stdout **file-change event** (§2.3), drains, fans the parsed events out to subscribers, and sleeps again. No timer, no tick. A subscriber with nothing happening costs zero wakeups, because nothing is written and no event fires.

This is the property herdr could not offer: its `events.wait` is a 100ms sleep loop over a ring buffer with **no fd to block on**. Camp has an fd — the self-pipe patrol already uses.

**But rev 2 cited evidence that does not exist.** `idle_campd_cpu_delta_zero_and_rss_under_20mb` (`perf_daemon.rs:232-251`) scaffolds an **empty camp**: no bead, no session — so zero patrol watches — and campd deregisters connections after each response, so zero standing clients. Citing that gate for "N tailed stdout files and N idle subscribers cost nothing" tests neither. What it *does* prove: the `camp.toml` config watcher is a live `notify` watcher for the entire idle window, so "a notify watcher costs 0.0% CPU" is genuinely demonstrated — the mechanism is fine; the fleet-scale claim is unproven.

**Obligation: extend the `make perf` idle gate to hold M quiescent workers with tailed stdout files and N connected subscribers** (fake workers, held open, no output), and assert the same 0.0% CPU / <20 MB RSS numbers. Then §4.3 is a measured property, not an argument.

### 4.4 `subscribe` is a new connection MODE, not just a new verb

Camp's socket is **one-shot** today: *"Send one request, read one response line"* (`socket.rs:314-318`), and `event_loop.rs:321-352` deregisters the connection after responding. `respond()` even documents its assumption — *"Responses are a few bytes; a WouldBlock here means the client is not reading."*

A long-lived, server-push, many-message subscription violates every one of those premises. It requires, and the implementation must provide — with numbers, because "bounded" without a number is how #55 happened:

- **per-connection output buffering** with a hard cap: `subscriber_buffer_bytes`, default **1 MiB** (the `MAX_REQUEST_BYTES` mold — `event_loop.rs:53`, 64 KiB, is campd's only other memory bound; this is its outbound sibling, and without it a subscription is campd's only unbounded allocation);
- **an explicit backpressure policy — and the cap is a STOP, not a kill** (AMENDED by cp-1; operator-approved. The original text read *"a subscriber whose buffer crosses the cap is dropped"*, and that is what the implementation deliberately does NOT do):
  - **`subscriber_buffer_bytes` bounds MEMORY.** When the buffer is full campd **stops framing** and **holds** the next complete line; the cursor does not advance. **Nothing is lost and nothing is dropped.**
  - **A subscriber is dropped when its PEER STOPS READING** — its socket has accepted **zero bytes** for `SUBSCRIBER_STALL_TIMEOUT` (30 s) with data buffered. That drop is **LOUD**: `subscriber.dropped`, naming the session and the **high-water mark**. campd never blocks on it, and events are never silently discarded. campd's loop is single-threaded — a stalled subscriber that campd waits on is a daemon-wide wedge (the bug class of issue #55).
  - ***Rationale (cp-1).*** During catch-up the producer is a **FILE read** (256 KiB/wake) and the consumer is a **socket** that accepts ~8 KiB (macOS's `net.local.stream.sendspace` is 8192) — **a file always outruns a socket**. So a buffer-SIZE kill drops **healthy, fast-reading clients that are merely BEHIND**: any client joining more than ~1 MiB behind the tail is killed *however fast it reads*, on a session with more than 1 MiB of output — which is ordinary. That breaks **§4.1's own late-joiner guarantee** (*"a late joiner gets history, then follows"*) and **§9's "never a silently truncated stream"**, and it reports the kill as backpressure about a client that was reading perfectly (invariant 3: an event must name its TRUE cause). campd still never blocks, memory is still bounded (`out` ≤ cap **and** `partial` ≤ cap), and a genuinely stalled peer is still dropped loudly.
  - ***Known residual, accepted for cp-1 and inherited by cp-2:*** a peer accepting **one byte per interval** clears the stall timer indefinitely and can hold a buffer and one of `MAX_SUBSCRIBERS` slots. It **is** reading, so it is not stalled — and any byte-rate floor is a policy number nobody has evidence for. A byte-rate floor, or a bound on time-at-cap, is the honest fix; it is recorded as a **cp-2 obligation**, not silently deferred. `camp watch` is precisely the thing an operator leaves open in a scrolled-back terminal all day.
- **a bounded hello:** `session.subscribe` answers with a first frame (subscription id + the cursor it will stream from) **within the existing one-shot `REQUEST_TIMEOUT`**, so `camp watch` against a **wedged** campd fails fast and loudly, exactly like every other verb. Only *after* the hello does the connection become timeout-exempt;
- **no `REQUEST_TIMEOUT` after the hello** (a quiet stream is not a wedged daemon).

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
2. Worker emits `can_use_tool` on the pipe; campd reads it (§2.3), marks the session **BLOCKED**, appends `permission.pending`, and **disarms the session's stall timer** (§5.3.3).
3. It surfaces in `camp watch` (and, later, wherever else you happen to be).
4. You answer — allow once, allow always, or deny with a reason.
5. campd **appends the decision to the ledger FIRST, then** writes the `control_response` and re-arms the stall timer; the worker continues.

The ledger-before-pipe ordering in step 5 is load-bearing, not stylistic: it makes *"the ledger shows an unanswered request"* mean *"the response was definitely never sent"* — which is what lets adoption (§5.3.4) kill safely. The reverse ordering leaves a window where a healthy, answered worker looks unanswered forever. (The decision event is who allowed what, when, for which bead. An agent asking to run `cargo publish` is a thing you want a record of.)

**If nobody answers, the worker stays blocked.** It does not time out into a default, it does not proceed, **and camp does not kill it for waiting** (§5.3.3 — rev 2 promised the first two while its own stall ladder broke the third). A permission question that answers itself is not a permission question. (Confirmed against the CLI: its `sendRequest` parks on a promise with **no timer** — the blocked-forever property is the CLI's, not camp's invention.)

#### 5.3.1 The flag is per-agent — added unconditionally, it would refuse every agent camp ships today

`--permission-prompt-tool stdio` only routes decisions the CLI **would otherwise have asked about**. Inside the **CLI binary** (like §2.2's `--verbose` gate — this is *not* observable in `sdk.mjs`, which only forwards `can_use_tool` to a user callback), `createCanUseTool` short-circuits first: `behavior === "allow"` returns immediately, `behavior === "deny"` returns immediately, and **only an "ask" reaches the parent.**

So `--permission-mode bypassPermissions`, or an `--allowedTools` that already covers everything, **suppresses the flow entirely** — and `bypassPermissions` + explicit `--allowedTools` is exactly camp's pinned worker config today (`assumption-findings.md`, F7). Two consequences, both mandatory:

- **camp adds `--permission-prompt-tool stdio` per agent, only when the agent's resolved permission mode can ask** (`default`, `acceptEdits`, `plan`). An agent resolved to `bypassPermissions` spawns exactly as today — no flag, no BLOCKED state, no behaviour change for any existing camp. Rev 2's unconditional flag would have made every current dispatch **fail §5.3.1's own spawn check**.
- **Camp refuses the incoherent combination.** Configuring the stdio flag together with a permission mode that can never ask is a **fail-fast error at spawn**, not a feature that quietly never fires. (Invariant 5.)

#### 5.3.2 A blocked worker must not hold a dispatch slot

`dispatch.rs:426,464` gate on `children.len() >= max_workers`, default **10** (`config.rs:62-64`). A BLOCKED worker is a live child. Left unhandled, **ten unanswered permission prompts silently deadlock the entire camp** — no new work dispatches, ever, and nothing says why.

**Decision: a BLOCKED worker does not count against `max_workers`**, and campd emits a `permission.pending` event on entry. Crossing a `max_blocked` threshold raises a loud, operator-visible saturation fault. The *decision* must never auto-answer; the *slot* must never be its hostage.

#### 5.3.3 BLOCKED disarms the stall ladder — a waiting worker is not a stalled worker

Rev 2 never said what BLOCKED does to patrol, and the default answer destroys the work: a worker parked on `can_use_tool` writes nothing and emits nothing, so its stall timer (default 10m) fires → `agent.stalled` → a nudge that cannot unblock a CLI parked on a promise → the ladder escalates → `LadderAction::Restart` → `kill_worker` → **SIGKILL of a worker that was doing exactly what §5.3 told it to do.** Rev 2's central promise — "it does not time out into a default" — was false: it timed out into a kill.

**Decision — the disarm/re-arm rule:**

- On `permission.pending` (campd reads the `can_use_tool`), the session's **stall timer is disarmed**. A BLOCKED session is exempt from the entire ladder: no nudge, no restart, no kill, for as long as the question stands. It is rendered in `camp watch` and counted by `max_blocked` (§5.3.2) — pressure on the *operator*, never on the worker.
- On the decision, the timer **re-arms from zero** (the worker is presumed working again).

**And the ladder's first act is always to drain the read channel.** There is an unavoidable race: the `can_use_tool` may be sitting unread in the stream file when the stall timer fires (a lost notify event — §2.3's bound). So any ladder action begins by draining that session's file to EOF; if a pending permission request emerges, the session transitions to BLOCKED, the timer disarms, and **no ladder action fires**. This single rule is simultaneously B1's safety net and B2's fix — the stall timer is how a lost event gets found, and finding it must never look like a stall.

*(Attended sessions were already annotate-only; this brings BLOCKED workers to the same footing for a different reason: patrol kills sessions that stop making progress, and a BLOCKED worker is making exactly as much progress as the operator lets it.)*

#### 5.3.4 A blocked worker cannot survive a campd restart, and must not pretend to

campd holds the worker's stdin. On a campd restart, **that pipe is gone** — even a BLOCKED state durably recorded in the ledger cannot be answered, because there is no channel left to answer on. The worker (which deliberately *outlives* campd, `spawn.rs:255`) would sit blocked forever, holding a worktree.

The CLI does offer redelivery — `initialize`'s response carries `pending_permission_requests` — but that serves a parent that can re-`initialize` **on a live stdin pipe**, and an adopted worker has none.

**Decision: on adoption, campd kills any worker whose ledger shows an unanswered permission request** — and §5.3's ledger-before-pipe ordering is what makes that safe: pending-in-ledger *proves* the response was never written, so this can never kill an answered, healthy worker. Specifically:

- the kill is evented with the reason **`"adoption: unanswerable permission request"`** — a named reason, greppable, never a generic crash. The rule covers a pending request **discovered after adoption** the same way: when post-adoption tailing (§2.3's resume-from-persisted-offset) surfaces a `can_use_tool` for an adopted worker, the resulting `permission.pending` lands on a session with no live stdin and takes this same named kill, not the generic stall ladder;
- **the bead re-hooks exactly as a patrol `restart` does** — back to ready, retry budget decremented, dispatchable to a fresh worker with a live stdin. (Without naming this, the bead lands in the dispatched-once dead zone and the work is stranded — the dispatchable set excludes ever-sessioned beads except through the restart path.)
- the inverse crash window (decision appended, campd died before the pipe write) leaves a worker parked with the ledger showing *answered*. That worker stays quiet, its re-armed stall timer fires, the ladder drains the channel, finds **no** pending request, and walks its normal bounded course to restart. Slow, evented, bounded — and it cannot be confused with the unanswered case.

**BLOCKED state lives in the ledger** (an event, not memory), so `camp watch` renders the same truth after a restart as before it — including the fact that the worker was killed, and why.

### 5.4 The overseer

An agent that holds the same socket: it can list sessions, read their streams, send them turns, and interrupt them. Camp already has an operator skill; under this design it becomes a **client of the control plane rather than a special case**, which is the only reason it is possible at all.

That it needs no new machinery is the strongest argument that the protocol is factored correctly.

## 6. What this is not

- **Not a terminal multiplexer.** No PTY, no panes, no attach-to-a-shell. If you want to *be* the agent, run `claude` yourself.
- **Not keystroke-level.** You send *turns* and *decisions*, not keypresses. There is no TUI to press keys into — that is the trade for having typed events instead of pixels, and it is the right one for an orchestrator.
- **Not a poller.** See §4.3 — including what the perf gate must grow before that section counts as proven.

## 7. Phases

0. **Fix #86 and build the read channel.** Pass `--verbose` (dispatch is broken on every clean machine without it). Byte-offset reads of the worker's stdout file, drained on every wake; `notify` → self-pipe as the latency path; Rescan/empty-path events drain everything (§2.3). Append-only stream files; reap-time disposal; `max_stream_bytes` ceiling. Stand up the **$0 tier** of the real-`claude` gate (§8). **Nothing else in this spec is buildable until campd can hear a worker.**
1. **Protocol + control module.** The socket verbs; the one module that owns the wire format, with pinned fixtures. `interrupt` and `send_turn` first — they are the smallest end-to-end slice through the whole stack, and `interrupt` is only verifiable once phase 0 lands (its `control_response` arrives on the read channel).
2. **`camp watch`.** The fleet view. Delivers most of the value on its own, and it is the cheapest thing to build.
3. **`can_use_tool` + the permission flow.** The highest-value single feature: it turns "unattended agent stalls forever on a permission it cannot get" into "the operator answers a question." Requires the ledger append, the `BLOCKED` state, the stall-timer disarm (§5.3.3), and the adoption rule (§5.3.4).
4. **`camp attach`.** The per-agent view: live stream, filter, replay, send-turn, interrupt.
5. **The overseer** as a first-class client.

## 8. Testing

**A fake worker validates camp's state machine. It can never validate the contract with a binary camp does not control.** #86 is the proof: camp's argv is rejected by the real CLI on any clean machine, every test is green, and no `#!/bin/sh` fake could ever have said so — because a fake ignores argv, ignores the protocol, and agrees with whatever camp does.

So the strategy is two-layered, and rev 1's *"this is better than testing against a real `claude`"* is deleted as false:

- **Fake workers for the state machine.** A `#!/bin/sh` worker can genuinely hold up its end — `while read -r line; do case … esac; done` on stdin, NDJSON on stdout — so it can emit a `can_use_tool` on demand, deterministically, and drive BLOCKED, the ledger append, and the fleet view. Cheap, hermetic, no API spend.
- **A real-`claude` compatibility gate, split by what it costs — because most of it is FREE.** Argv rejection happens at CLI validation *before any turn* (that is #86's signature); `initialize` and an `interrupt` sent before any turn are CLI-local. **The $0 tier** — spawn the real CLI, assert the argv is accepted, the `initialize` handshake round-trips, and a pre-turn `interrupt` is acknowledged — costs no API call and runs wherever a pinned `claude` binary exists, often. **The paid tier** — a forced `can_use_tool`, its `control_response`, and the worker's continuation — needs a real turn and rides `make e2e` (opt-in, local-only, the sanctioned envelope), required before a release. Rev 2 made the whole gate release-blocking as one unit, which made a paid run a blocker by side effect; the split keeps the release bar without the bill.
- **Pin the tested `claude` version and fail loudly on an unpinned one**, exactly as `ci/gc-compat/GASCITY_REF` pins the gc compiler for invariant 6.

Without the real layer, §2.1's mitigations are theatre: fixtures pin what camp *sends and parses*, never what the CLI *accepts and emits*. A release that renames a subtype leaves every fixture green and every worker broken.

State-machine tests that must exist, each dying against a mutation of what it guards:

- **Blocked-forever:** a worker that emits `can_use_tool` and receives no answer remains blocked, does not time out, does not proceed — **and is not killed**: advance past the stall threshold and assert no ladder action fired (§5.3.3's disarm, the test rev 2 could not have passed).
- **Ladder-drains-first:** a `can_use_tool` written to the stream file with its notify event suppressed; the stall timer fires; assert the session transitions to BLOCKED and no nudge/restart happens.
- **Read-on-wake:** append a line to a tailed file with events suppressed; trigger an unrelated wake (a socket request); assert the line is consumed. Then deliver a synthetic `Rescan`/empty-paths event and assert every tailed file is drained.
- **Append-only cursors:** kill campd mid-stream, restart, re-subscribe from the prior byte offset; assert no loss and no duplication. Assert the ceiling: a stream crossing `max_stream_bytes` fails the session loudly with the named event.
- **Adoption:** ledger shows pending + no live stdin ⇒ the worker is killed with reason `"adoption: unanswerable permission request"` and the bead is dispatchable again; ledger shows *answered* + quiet worker ⇒ no adoption kill (the stall ladder owns it).
- **Subscriber backpressure** (per §4.4 as amended by cp-1): a subscriber that **stops reading** — zero bytes accepted for `SUBSCRIBER_STALL_TIMEOUT` with data buffered — is dropped with the `subscriber.dropped` event, naming the high-water mark; campd never blocks. A subscriber that is merely **BEHIND** is never dropped: the `subscriber_buffer_bytes` cap is a **STOP** (campd holds the line and stops framing), not a kill. The hello arrives within `REQUEST_TIMEOUT` even against a busy daemon.
- **Invariant 1:** the **extended** perf gate (§4.3): M quiescent workers with tailed stdout files, N connected subscribers, zero activity ⇒ 0.0% CPU delta, <20 MB RSS.

## 9. Decisions that were open questions

Rev 1 parked these. Each is a prerequisite, not a curiosity, so each is decided here.

- **The `initialize` handshake.** *Optional* for a parent that only wants `interrupt` + `can_use_tool` — the `stdio` handler is wired from argv at startup, not gated on the handshake. **Camp sends it anyway**, because its response carries `pending_permission_requests`, and that is the only redelivery mechanism there is.
- **`request_user_dialog`.** The CLI genuinely sends it under `stdio`. Camp **answers it with a deterministic `control_response{subtype:"error"}`** ("interactive dialogs are not supported"). It is neither ignored (which §2.1 would raise as a protocol fault) nor left to hang a worker forever.
- **Event history bound.** Rev 2 said "a byte cap with rotation"; §2.3 shows that corrupts a live channel. The stream file is **append-only until reap**, bounded by `max_stream_bytes` (loud failure) and disposed/compressed at session end. **`session.subscribe` cursors are byte offsets into the stream file** — which makes them durable across a campd restart for free. A cursor into a reaped (disposed) stream is an explicit error, never a silently truncated stream.
- **Multiple deciders.** **First answer wins** (ledger append order decides — §5.3's ordering makes the ledger the serialization point), and the ledger records who answered. Losing deciders get an explicit "already decided by X" response rather than silence.

## 10. Open questions

*(None that block phase 0 or 1.)*

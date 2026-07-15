# cp-5 — The Overseer: the operator skill as a first-class control-plane client — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the existing `camp:operator` skill into a first-class control-plane client that drives every §5.4 overseer action — `sessions.list`, `session.subscribe`, `session.send_turn`, `session.interrupt`, `session.permission_decision` — through the campd socket alone, with no private paths (no worker stream-file tails, no pids), and prove it with an instrument that can FALSIFY the socket-only claim, not merely assert it.

**Architecture:** The overseer is the human's (or an agent's) own Claude Code session running `camp` CLI verbs. Each verb is a stateless socket client (control-plane spec §4.2). Two of the five §5.4 actions have a socket verb but no agent-usable one-shot CLI client yet — `sessions.list` (only `camp watch`, a blocking stream, and `camp top`, aggregate counts, exist) and `session.interrupt` (only reachable inside `camp attach`'s interactive `/interrupt` loop). cp-5 adds two thin one-shot clients — `camp sessions` and `camp interrupt` — modelled exactly on cp-3's `camp decide` (a one-shot pure client over `session.permission_decision`). The remaining three actions reuse the already-merged `camp attach` (cp-4), `camp nudge` (cp-1's `session.send_turn`), and `camp decide` (cp-3). The skill is then rewritten to name these verbs and to state the reach-a-worker-only-through-the-socket discipline; three test layers bind the skill to that discipline: a skill-pinning test, a strengthened zero-agent-definitions policy test, and a fake-fleet integration test whose falsification arm proves the socket is both NECESSARY (campd down ⇒ loud failure, never a private-path read) and SUFFICIENT (worker stream files + campd.log made unreadable ⇒ every verb still works over the socket).

**Tech Stack:** Rust (the `camp` binary crate), clap subcommands, the existing `crates/camp/src/daemon/socket.rs` `Request`/`Response` wire types, the `tests/fake-agent.sh` fake worker, the `tests/control.rs` end-to-end socket harness idioms, and the `plugin/skills/operator/SKILL.md` markdown skill.

## Global Constraints

Copied verbatim from AGENTS.md and the control-plane spec; every task's requirements implicitly include this section.

- **inv-1 Idle is free.** No ticks, no polling loops anywhere. cp-5 adds only one-shot request/response clients and bounded-read test children — it introduces no loop that wakes without an event. (`AGENTS.md:11-13`)
- **inv-4 Six primitives, zero roles in code.** "If a line of Rust contains a role name or a judgment call, it is a bug. campd moves work; it never reasons about it." The plugin ships zero agent definitions. (`AGENTS.md:19-21`; master design §11 `docs/design/2026-07-05-gas-camp-design.md:694` — "It ships no agent definitions. Roles are pack content. Same law as the city: if the machinery mentions a role, it is a bug.")
- **inv-5 Fail fast.** No fallbacks, no silenced errors, no placeholders. No panics in library code (clippy `unwrap_used`/`expect_used`/`panic` denied; `unsafe_code` forbidden). Every error surfaces to the caller or lands in the ledger. Test modules may `#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]` exactly as every existing `tests/*.rs` does. (`AGENTS.md:22-25`)
- **§4 The no-private-paths rule (the phase's spine).** "campd's socket is the control plane, and it is the only path to a worker. Every client goes through it. No client gets a private path (no tailing files, no signalling pids)." (`docs/superpowers/specs/2026-07-12-camp-control-plane-design.md:135`) §4.2 rule 1: "Sessions are addressed by name, never by pid or file path." Rule 2: "campd owns the truth; clients are stateless renderers." (`spec:151-152`)
- **§5.4 The overseer (verbatim scope).** "An agent that holds the same socket: it can list sessions, read their streams, send them turns, and interrupt them. Camp already has an operator skill; under this design it becomes a client of the control plane rather than a special case… That it needs no new machinery is the strongest argument that the protocol is factored correctly." (`spec:273-277`)
- **Wire shapes are frozen; do not touch them.** `sessions.list`, `session.subscribe`, `session.send_turn`, `session.interrupt`, `session.permission_decision`, and the `SessionInfo`/`Response` structs are already merged and pinned by cp-1/cp-3/cp-4. cp-5 CONSUMES them and adds NO protocol field, NO daemon handler, NO new verb.
- **TDD, strictly.** Write the failing test, run it, watch it fail, implement, watch it pass, commit. (`AGENTS.md:36-37`)
- **Never commit to main; no co-author lines.** Work lands on `cp-5-overseer` via PR. (`AGENTS.md:38-39`)

## The five §5.4 actions → the concrete socket verb, its frozen wire shape, and the CLI client that drives it

This is the mapping the plan gate checks. Every action reaches the worker THROUGH THE SOCKET; none reads a stream file or a pid.

| §5.4 action | socket verb (frozen) | `Request` variant → `Response` variant (`socket.rs`) | CLI client the skill names | status |
|---|---|---|---|---|
| list sessions | `sessions.list` | `Request::SessionsList` → `Response::SessionsList { ok, sessions: Vec<SessionInfo> }` (`socket.rs:57`, `:140-143`); `SessionInfo { name, agent, rig, bead, state, blocked }` (`socket.rs:112-130`) — **no pid, by design** (`socket.rs:109-111`) | **`camp sessions [--json]`** — NEW one-shot client (Task 1) | verb merged (cp-1); client added by cp-5 |
| read their streams | `session.subscribe` | `Request::SessionSubscribe { session, cursor }` → `Response::Subscribed { ok, v, subscription, cursor }` then server-push frames (`socket.rs:67-71`, `:175-180`) | `camp attach <session> [--only] [--tail] [--from]` (cp-4) | merged (cp-4); reused |
| send them turns | `session.send_turn` | `Request::SessionSendTurn { session, text }` → `Response::SendTurn { ok, via }` (`socket.rs:49-52`, `:167-170`) | `camp nudge <session> "<text>"` (cp-1) | merged (cp-1); reused |
| interrupt them | `session.interrupt` | `Request::SessionInterrupt { session }` → `Response::Interrupt { ok, request_id }` (`socket.rs:80-82`, `:209-212`) | **`camp interrupt <session>`** — NEW one-shot client (Task 2) | verb merged (cp-1); client added by cp-5 |
| answer a permission | `session.permission_decision` | `Request::SessionPermissionDecision { session, request_id, decision, message }` → `Response::PermissionDecided { ok, request_id, decision }` (`socket.rs:90-96`, `:200-204`) | `camp decide <session> <request_id> allow\|allow_always\|deny [--reason]` (cp-3) | merged (cp-3); reused |

**Why `camp sessions` and `camp interrupt` are new (contract deviation — additive only; see the dedicated section below).** The socket verbs exist, but the only merged CLI surfaces for them are the human-facing streaming views (`camp watch` blocks forever; `camp attach`'s `/interrupt` lives inside an interactive stdin steering loop) and the aggregate `camp top` (counts, not a per-session list). §5.4's overseer is an *agent* that needs a one-shot snapshot to discover session NAMES and a one-shot stop — the exact shape cp-3 already shipped for the permission verb (`camp decide`). Both new clients are pure `socket::require` round-trips; they add zero protocol/daemon machinery, so §5.4's "it needs no new machinery" (about the PROTOCOL) still holds.

---

## File Structure

- `crates/camp/src/cmd/sessions.rs` — **create.** `camp sessions [--json]`: one-shot `sessions.list` socket client; renders the per-session table (human) or the `Vec<SessionInfo>` JSON (machine). One responsibility: turn one `sessions.list` round-trip into output.
- `crates/camp/src/cmd/interrupt.rs` — **create.** `camp interrupt <session>`: one-shot `session.interrupt` socket client; prints the ack `request_id`. Mirror of `cmd/decide.rs`.
- `crates/camp/src/main.rs` — **modify.** Register the two `pub mod`s (`:7-38` block), add two clap `Command` variants, add two dispatch arms.
- `plugin/skills/operator/SKILL.md` — **modify.** Rewrite the mental-model and Verbs sections so the operator is a control-plane client: name `camp sessions`, `camp attach`, `camp nudge`, `camp interrupt`, `camp decide`; state the reach-a-worker-only-through-the-socket discipline; keep every load-bearing line the pinning test guards.
- `crates/camp/tests/plugin_operator_skill.rs` — **modify.** Extend the skill-pinning test to require the control-plane verbs and the no-private-paths discipline line.
- `crates/camp/tests/plugin_policy.rs` — **modify.** Strengthen the zero-agent-definitions guard (an `agent.toml` anywhere under `plugin/`, not only an `agents/` dir) and prove it goes RED.
- `crates/camp/tests/overseer.rs` — **create.** The exit-criteria integration test: every §5.4 action driven as the real CLI subprocess the skill names, against a live fake fleet over the socket (sufficiency/positive arm), PLUS the no-private-paths falsification instrument (necessity arm: campd down ⇒ loud fail; sufficiency arm: worker stream files + campd.log unreadable ⇒ still works) PLUS the static source-audit tripwire.

---

### Task 1: `camp sessions` — the `sessions.list` snapshot client

**Files:**
- Create: `crates/camp/src/cmd/sessions.rs`
- Modify: `crates/camp/src/main.rs:7-38` (add `pub mod sessions;`), the `Command` enum (add a `Sessions` variant), the dispatch `match` (add the arm)
- Test: unit tests inside `crates/camp/src/cmd/sessions.rs` (`render` is pure); the socket round-trip is covered by the integration test in Task 5

**Interfaces:**
- Consumes: `socket::require(camp, &Request::SessionsList) -> Result<Response>` (`socket.rs:436`); `Response::SessionsList { ok, sessions }` (`socket.rs:140`); `SessionInfo { name, agent, rig, bead, state, blocked }` (`socket.rs:112`, already `#[derive(Serialize)]`).
- Produces: `pub fn run(camp: &CampDir, json: bool) -> anyhow::Result<()>`; `fn render(sessions: &[SessionInfo]) -> String` (pure, unit-tested).

- [ ] **Step 1: Write the failing unit test for `render`**

Create `crates/camp/src/cmd/sessions.rs` with ONLY the test module and the two function signatures (bodies `todo!()`), so the test compiles and fails at runtime:

```rust
//! `camp sessions [--json]` (control-plane §5.4): the overseer's one-shot
//! snapshot of the live fleet — one row per session, addressed BY NAME (§4.2),
//! sourced ONLY from the socket's `sessions.list` verb. The non-streaming
//! sibling of `camp watch`: `watch` is the human's live second-monitor view;
//! `sessions` is the snapshot an AGENT overseer reads once and moves on from.
//!
//! A PURE CLIENT (design §4): it reaches the fleet ONLY through the socket. It
//! never opens a worker's stdout stream file under `sessions/`, never reads a
//! pid — a down campd is a loud, actionable error (the socket verb's own
//! `CampdNotRunning`), never a silent read of on-disk state.

use anyhow::{Result, bail};

use crate::campdir::CampDir;
use crate::daemon::socket::{self, Request, Response, SessionInfo};

pub fn run(camp: &CampDir, json: bool) -> Result<()> {
    todo!()
}

/// One line per session: `NAME  AGENT  RIG  BEAD  STATE`, where a BLOCKED
/// session (§5.3) renders `BLOCKED` in the STATE column — the state that
/// matters and that must be impossible to miss (§5.1).
fn render(sessions: &[SessionInfo]) -> String {
    todo!()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn info(name: &str, state: &str, blocked: bool) -> SessionInfo {
        SessionInfo {
            name: name.to_owned(),
            agent: "dev".to_owned(),
            rig: Some("t3".to_owned()),
            bead: Some("t3-1".to_owned()),
            state: state.to_owned(),
            last_activity: "2026-07-15T00:00:00Z".to_owned(),
            blocked,
        }
    }

    #[test]
    fn render_shows_one_row_per_session_and_surfaces_blocked() {
        let out = render(&[
            info("t3/dev/1", "working", false),
            info("t3/dev/2", "working", true),
            info("t3/dev/3", "stalled", false),
        ]);
        // one row per session, by name
        assert!(out.contains("t3/dev/1"));
        assert!(out.contains("t3/dev/2"));
        assert!(out.contains("t3/dev/3"));
        // BLOCKED overrides the working/stalled state and is spelled loudly
        assert!(out.contains("BLOCKED"), "blocked session must render BLOCKED: {out}");
        // the non-blocked states survive
        assert!(out.contains("working"));
        assert!(out.contains("stalled"));
    }

    #[test]
    fn render_of_an_empty_fleet_is_a_clear_single_line() {
        let out = render(&[]);
        assert!(out.to_lowercase().contains("no live session"), "got: {out}");
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p camp --bin camp cmd::sessions -- --nocapture`
Expected: FAIL — `render` panics with `not yet implemented` (`todo!()`).

- [ ] **Step 3: Implement `render` and `run`**

Replace the two `todo!()` bodies:

```rust
pub fn run(camp: &CampDir, json: bool) -> Result<()> {
    // A PURE CLIENT (design §4.3, mirror of `camp decide`): it never starts
    // campd, and a down campd is a loud error — the fleet lives behind the
    // socket, never in a file this client is allowed to read.
    let response = socket::require(camp, &Request::SessionsList)?;
    match response {
        Response::SessionsList { sessions, .. } => {
            if json {
                // The machine read (operator skill's `--json` discipline): the
                // exact wire `SessionInfo` vec, verbatim.
                println!("{}", serde_json::to_string(&sessions)?);
            } else {
                print!("{}", render(&sessions));
            }
            Ok(())
        }
        Response::Error { error, .. } => bail!("{error}"),
        other => bail!("unexpected response to sessions.list: {other:?}"),
    }
}

fn render(sessions: &[SessionInfo]) -> String {
    if sessions.is_empty() {
        return "no live sessions\n".to_owned();
    }
    let mut out = String::from("NAME                 AGENT            RIG        BEAD          STATE\n");
    for s in sessions {
        let state = if s.blocked { "BLOCKED" } else { s.state.as_str() };
        out.push_str(&format!(
            "{:<20} {:<16} {:<10} {:<13} {}\n",
            s.name,
            s.agent,
            s.rig.as_deref().unwrap_or("-"),
            s.bead.as_deref().unwrap_or("-"),
            state,
        ));
    }
    out
}
```

- [ ] **Step 4: Run the unit tests to verify they pass**

Run: `cargo test -p camp --bin camp cmd::sessions -- --nocapture`
Expected: PASS (both tests).

- [ ] **Step 5: Wire the subcommand into `main.rs`**

In the `mod cmd { … }` block (`main.rs:7-38`), add — alphabetically, after `pub mod session;` (`:31`):

```rust
    pub mod sessions;
```

In the `enum Command` (after the `Session { … }` variant, `main.rs:336-340`), add:

```rust
    /// List every live session by name — the overseer's one-shot snapshot of
    /// the fleet (control-plane §5.4), sourced only from the socket's
    /// `sessions.list` verb. `--json` emits the raw SessionInfo array. campd
    /// must be running (a pure client — no file reads, no pids).
    Sessions {
        /// Emit the live-session array as one JSON line (machine read).
        #[arg(long)]
        json: bool,
    },
```

In the dispatch `match` (next to the `Command::Top` arm, `main.rs:915`), add:

```rust
        Command::Sessions { json } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            cmd::sessions::run(&camp, json)
        }
```

- [ ] **Step 6: Confirm the whole crate builds and the CLI verb exists**

Run: `cargo build -p camp && cargo run -p camp -- sessions --help`
Expected: build OK; help text for `camp sessions` prints with the `--json` flag.

- [ ] **Step 7: Commit**

```bash
git add crates/camp/src/cmd/sessions.rs crates/camp/src/main.rs
git commit -m "feat(cp-5): camp sessions — one-shot sessions.list socket client"
```

---

### Task 2: `camp interrupt` — the one-shot `session.interrupt` client

**Files:**
- Create: `crates/camp/src/cmd/interrupt.rs`
- Modify: `crates/camp/src/main.rs` (register `pub mod interrupt;`, add the `Interrupt` variant, add the dispatch arm)
- Test: the socket round-trip is covered by Task 5's integration test; this task adds no unit test (the client is a straight-line `socket::require` match with no pure logic to unit-test — mirroring `cmd/decide.rs`, which has none either).

**Interfaces:**
- Consumes: `socket::require(camp, &Request::SessionInterrupt { session }) -> Result<Response>`; `Response::Interrupt { ok, request_id }` (`socket.rs:209`).
- Produces: `pub fn run(camp: &CampDir, session: String) -> anyhow::Result<()>`.

- [ ] **Step 1: Write the failing integration expectation (deferred to Task 5) — here, write the module skeleton that must compile**

Create `crates/camp/src/cmd/interrupt.rs`:

```rust
//! `camp interrupt <session>` (control-plane §5.4): the overseer's one-shot
//! stop of a live worker's turn — the non-interactive sibling of `camp attach`'s
//! `/interrupt`, so an AGENT overseer can interrupt a named session without
//! entering the interactive steering loop.
//!
//! A PURE CLIENT (design §4, exact mirror of `camp decide`): it reaches the
//! worker ONLY through the socket's `session.interrupt` verb. There is NO
//! resume path — a turn can be stopped only through the pipe campd holds
//! (spec §4.1 D1: campd acks as soon as the control line is in the pipe; the
//! worker's `control_response` lands in the ledger). A down campd is therefore
//! a loud, actionable error, never a silent no-op and never a pid signal.

use anyhow::{Result, bail};

use crate::campdir::CampDir;
use crate::daemon::socket::{self, Request, Response};

pub fn run(camp: &CampDir, session: String) -> Result<()> {
    let response = socket::require(
        camp,
        &Request::SessionInterrupt {
            session: session.clone(),
        },
    )?;
    match response {
        Response::Interrupt { request_id, .. } => {
            println!(
                "interrupt {request_id} is in {session}'s pipe; the worker's ack \
                 lands in the ledger as control.responded"
            );
            Ok(())
        }
        Response::Error { error, .. } => bail!("{error}"),
        other => bail!("unexpected response to the interrupt: {other:?}"),
    }
}
```

- [ ] **Step 2: Wire the subcommand into `main.rs`**

In `mod cmd { … }`, add alphabetically (after `pub mod import;`, `:19`):

```rust
    pub mod interrupt;
```

In `enum Command` (place it near `Attach`/`Watch`, after the `Attach { … }` variant), add:

```rust
    /// Interrupt a live worker's current turn (control-plane §5.4) — a one-shot
    /// over the socket's `session.interrupt` verb. The non-interactive sibling
    /// of `camp attach`'s `/interrupt`. campd must be running (a pure client —
    /// a turn is stoppable only through the pipe campd holds).
    Interrupt {
        /// The session NAME (from `camp sessions` / `camp watch`).
        session: String,
    },
```

In the dispatch `match` (next to the `Command::Attach` arm), add:

```rust
        Command::Interrupt { session } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            cmd::interrupt::run(&camp, session)
        }
```

- [ ] **Step 3: Run to verify the crate builds and the verb exists**

Run: `cargo build -p camp && cargo run -p camp -- interrupt --help`
Expected: build OK; help text for `camp interrupt <session>` prints.

- [ ] **Step 4: Commit**

```bash
git add crates/camp/src/cmd/interrupt.rs crates/camp/src/main.rs
git commit -m "feat(cp-5): camp interrupt — one-shot session.interrupt socket client"
```

---

### Task 3: Rewrite the operator skill into a control-plane client (and pin it)

The skill IS the contract (its own test file says so, `plugin_operator_skill.rs:2-5`). cp-5 makes the operator a client of the control plane: it names the five §5.4 socket clients and states the reach-a-worker-only-through-the-socket discipline, while keeping every load-bearing line the existing pinning test guards (mental model, deliverable model, output discipline, don't-poll).

**Files:**
- Modify: `plugin/skills/operator/SKILL.md`
- Test: `crates/camp/tests/plugin_operator_skill.rs`

**Interfaces:**
- Consumes: nothing (markdown + a string-matching test).
- Produces: a skill whose §6 Verbs names `camp sessions`, `camp attach`, `camp nudge`, `camp interrupt`, `camp decide`; a new discipline line the pinning test asserts.

- [ ] **Step 1: Write the failing pinning-test additions**

In `crates/camp/tests/plugin_operator_skill.rs`, ADD two tests (do not weaken the existing four):

```rust
#[test]
fn operator_skill_names_the_control_plane_verbs() {
    let s = operator_skill();
    for needle in [
        "camp sessions",  // §5.4 list sessions (sessions.list)
        "camp attach",    // §5.4 read their streams (session.subscribe)
        "camp nudge",     // §5.4 send them turns (session.send_turn)
        "camp interrupt", // §5.4 interrupt them (session.interrupt)
        "camp decide",    // §5.3 answer a permission (session.permission_decision)
    ] {
        assert!(
            s.contains(needle),
            "operator skill must name the control-plane verb `{needle}`"
        );
    }
}

#[test]
fn operator_skill_states_the_no_private_paths_discipline() {
    let s = operator_skill();
    // The reach-a-worker-only-through-the-socket rule (§4): the skill must tell
    // the operator NOT to tail a worker's stream file or reach it by pid.
    assert!(
        s.contains("socket"),
        "operator skill must name the socket as the only path to a worker"
    );
    for needle in ["stream file", "pid"] {
        assert!(
            s.contains(needle),
            "operator skill must forbid reaching a worker by `{needle}` (§4)"
        );
    }
}
```

- [ ] **Step 2: Run to verify the new tests fail**

Run: `cargo test -p camp --test plugin_operator_skill -- --nocapture`
Expected: FAIL — the two new tests fail (the current skill names none of `camp sessions`/`camp attach`/`camp interrupt`/`camp decide`, nor "stream file"/"pid").

- [ ] **Step 3: Rewrite the skill**

Edit `plugin/skills/operator/SKILL.md`. Keep the frontmatter and every needle the existing tests guard (`campd`, `enqueue`, `camp/<bead>`, `no remote`, `shipped`, `never paste`, `--json`, `poll`, `--wait`, `camp sling`, `camp show`, `camp nudge`, `camp top`). Make these two edits:

(a) In the mental-model bullet that already says the operator does not reconstruct campd's state "from `campd.log`, the `sessions/` dir, or the process table" (currently `SKILL.md:16-19`), extend it so it covers the live-worker case too. Replace that bullet with:

```markdown
- **campd is the sole dispatcher.** `camp sling "<title>"` only **enqueue**s
  one bead; campd immediately spawns a headless-but-present worker (spec
  §8.4). You spawn nothing, and you do not reconstruct what campd is doing
  from `campd.log`, the `sessions/` dir, or the process table — the ledger is
  the story. **The socket is the only path to a worker.** To watch, steer,
  interrupt, or answer a live worker you go through the `camp` control-plane
  verbs below, which reach it only over campd's socket (spec §4). You never
  tail a worker's stream file and never reach it by pid — those are private
  paths the control plane exists to abolish, and a client that used one could
  not follow a worker onto another machine (§4.2).
```

(b) Replace the entire `## 6. Verbs` section (`SKILL.md:60-69`) with a control-plane-client verb list:

```markdown
## 6. Verbs — every one a socket client (spec §4)

Dispatch & inspect (the loop of §2):

- `camp sling "<title>" [--agent A] [--rig R]` — enqueue one bead (`/sling`).
- `camp show <bead> [--wait] [--json]` — one bead's state; `--wait` blocks
  until it closes, `--json` for machine reads.
- `camp top` — fleet counts snapshot (`/status`); `camp events` — the whole
  event log (`/events`) — read it, don't paste it.

The overseer's control plane (spec §5.4 — each drives one socket verb, and
ONLY the socket; none reads a stream file or a pid):

- `camp sessions [--json]` — one-shot snapshot of every LIVE session by name,
  with its state and whether it is **BLOCKED** on a permission (verb:
  `sessions.list`). This is how you learn the session names the verbs below
  take. `camp watch` is the same fleet, streamed live for a human on a second
  monitor (verb: `fleet.subscribe`); `camp sessions` is the snapshot an agent
  reads once.
- `camp attach <session> [--only text|tools|edits|failures] [--tail]` — read
  one worker's live typed event stream: tool calls, results, assistant text,
  usage (verb: `session.subscribe`). Filter and replay; detach freely.
- `camp nudge <session> "<message>"` — inject one user turn into a live
  worker's campd-held stdin (verb: `session.send_turn`); it lands in the
  worker's current conversation now (`/nudge`).
- `camp interrupt <session>` — stop a live worker's current turn (verb:
  `session.interrupt`). The ack's request id lands in the ledger.
- `camp decide <session> <request_id> allow|allow_always|deny [--reason ...]`
  — answer a worker's `can_use_tool`: the BLOCKED row in `camp sessions` /
  `camp watch` carries the `request_id` (verb: `session.permission_decision`).
  A `deny` needs `--reason` (the worker sees it).
- `camp adopt` — reconcile the session registry against reality (`/adopt`).
```

- [ ] **Step 4: Run the full skill-pinning test suite to verify green**

Run: `cargo test -p camp --test plugin_operator_skill`
Expected: PASS — all six tests (four original + two new).

- [ ] **Step 5: Commit**

```bash
git add plugin/skills/operator/SKILL.md crates/camp/tests/plugin_operator_skill.rs
git commit -m "feat(cp-5): operator skill becomes a control-plane client (spec §5.4)"
```

---

### Task 4: Strengthen the zero-agent-definitions policy test (master §11 companion clause)

The guard already exists (`plugin_policy.rs:26-48`): no `agents/` directory under `plugin/`, and `plugin.json` declares no `agents` component. cp-5 (a) proves it goes RED when an agent def is added, and (b) closes the one hole the current guard leaves — an agent definition is, per compat §5.1, a **directory with an `agent.toml`**, and a stray `agent.toml` NOT inside a dir literally named `agents/` slips past the current check.

**Files:**
- Modify: `crates/camp/tests/plugin_policy.rs`

**Interfaces:**
- Consumes: the `walk(dir, out)` helper already in the file (`plugin_policy.rs:14-24`).
- Produces: a strengthened `plugin_ships_zero_agent_definitions` that also rejects any `agent.toml` anywhere under `plugin/`.

- [ ] **Step 1: Write the failing strengthening assertion**

In `crates/camp/tests/plugin_policy.rs`, inside `plugin_ships_zero_agent_definitions`, after the existing `agents/`-directory loop (`:33-39`), add:

```rust
    // Compat §5.1: an agent definition IS a directory with an `agent.toml`.
    // The `agents/`-dir check above misses a bare agent.toml dropped elsewhere
    // under plugin/ — so reject the file itself, anywhere. This is the tripwire
    // that turns "the machinery mentions a role" into a RED build (§11).
    for p in &paths {
        assert!(
            p.file_name().and_then(|n| n.to_str()) != Some("agent.toml"),
            "the plugin must ship no agent definition (agent.toml): {}",
            p.display()
        );
    }
```

- [ ] **Step 2: Run to verify the whole policy test still passes (no agent.toml exists yet)**

Run: `cargo test -p camp --test plugin_policy`
Expected: PASS — the new assertion is satisfied (there is no `agent.toml` under `plugin/`), the strengthening compiles.

- [ ] **Step 3: PROVE it goes RED — the falsification of the policy guard**

This is a manual falsification, run once and recorded, not committed:

```bash
# a) an agent.toml smuggled into an existing skill dir (defeats the old agents/ check)
mkdir -p plugin/skills/rogue && printf 'model = "sonnet"\n' > plugin/skills/rogue/agent.toml
cargo test -p camp --test plugin_policy 2>&1 | tail -20   # EXPECT: FAIL on agent.toml
rm -rf plugin/skills/rogue

# b) the classic form: an agents/ directory
mkdir -p plugin/agents && printf '# rogue role\n' > plugin/agents/dev.md
cargo test -p camp --test plugin_policy 2>&1 | tail -20   # EXPECT: FAIL on the agents/ dir
rm -rf plugin/agents

# c) the manifest key
# (temporarily add `"agents": "./agents"` to plugin/.claude-plugin/plugin.json,
#  run the test, EXPECT FAIL on the plugin.json assertion, then revert)
```

Record in the PR description that all three forms went RED and the tree is clean again.

- [ ] **Step 4: Re-run to confirm green after cleanup**

Run: `cargo test -p camp --test plugin_policy && git status --porcelain plugin/`
Expected: PASS; `git status` shows NO stray `plugin/` changes (only `tests/plugin_policy.rs` is modified).

- [ ] **Step 5: Commit**

```bash
git add crates/camp/tests/plugin_policy.rs
git commit -m "test(cp-5): plugin policy rejects a stray agent.toml, not only agents/ (§11)"
```

---

### Task 5: The overseer exit-criteria integration test — every §5.4 action against a fake fleet, over the socket

This is the exit criterion: the overseer performs every §5.4 action against a FAKE fleet through the socket alone. The test drives the **actual CLI subprocesses the skill names** (`camp sessions`, `camp attach`, `camp nudge`, `camp interrupt`, `camp decide`) against a live campd + fake workers. Binding: Task 3's pinning test proves the skill NAMES these verbs; this test proves each named verb PERFORMS its §5.4 action over the socket. Together they mean "the overseer skill performs every §5.4 action" — without brittle markdown execution.

#### Fixture-dimension table — what the fake fleet DOES and does NOT span (so no assertion passes vacuously)

| dimension | spanned by this test | deliberately NOT spanned (owned elsewhere) |
|---|---|---|
| session states | `working`, `stalled`, `BLOCKED` (permission pending), and a finished/exited worker | — |
| fleet cardinality | **≥2 concurrent live sessions**, so `sessions.list` returns multiple rows and every steer verb must select by NAME (proves name-addressing, not positional/pid) | very large fleets (perf gate's job, §4.3) |
| §5.4 verbs | all five: `sessions.list`, `session.subscribe`, `session.send_turn`, `session.interrupt`, `session.permission_decision` | `fleet.subscribe` (cp-2's `camp watch` surface, not a §5.4 overseer action) |
| decide outcomes | `allow` on a genuinely BLOCKED worker (the worker then continues) | `allow_always` / `deny` re-validation and multi-decider races (cp-3's own suite) |
| worker binary | `tests/fake-agent.sh` only (the state-machine layer, §8) | the real `claude` `$0`/paid tiers (§8's split — cp-5 adds no compat-gate obligation) |
| transport | the unix socket | remote/other transports (§4.2 rule 3 — out of scope) |
| private-path artifacts | present AND poisoned/absent in Task 6's falsification arm (stream files, campd.log, pids) | — |
| campd continuity | one live campd | restart/adoption across a verb (cp-3's `a_campd_restart_across_an_in_flight_interrupt` and adoption suite) |

**Files:**
- Create: `crates/camp/tests/overseer.rs`
- Test harness: a compact copy of the `control.rs` idioms (`BIN`, `munge`, `scaffold`, `Daemon::spawn`, `camp`, `camp_ok`, `dispatch_one`, `wait_for_stdout`, `events_json`, `wait_until`), because Rust integration-test binaries do not share helpers — `control.rs` inlines its own for the same reason (`control.rs:10`).

**Interfaces:**
- Consumes: the `camp` binary (`env!("CARGO_BIN_EXE_camp")`), `tests/fake-agent.sh` and its env knobs (`FAKE_AGENT_CONTROL_LOOP`, `FAKE_AGENT_CAN_USE_TOOL`, `FAKE_AGENT_LINGER_ON_EOF`, `FAKE_AGENT_CAN_USE_TOOL_REQ`).
- Produces: the exit-criteria proof; the harness Task 6 reuses.

- [ ] **Step 1: Write the harness + the first failing test (`sessions.list` sees the whole fleet by name)**

Create `crates/camp/tests/overseer.rs`. The harness mirrors `control.rs:21-262` — reproduce those helpers verbatim into this file (they are already the pinned idiom; do not re-derive). Then the first test:

```rust
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! cp-5 exit criterion (control-plane §5.4): the overseer performs EVERY §5.4
//! action against a fake fleet THROUGH THE SOCKET ALONE, driving the exact
//! `camp` CLI verbs the operator skill names. The no-private-paths instrument
//! (Task 6) proves the socket is both NECESSARY and SUFFICIENT.

// ── harness: copied verbatim from tests/control.rs (BIN, munge, stdout_path,
//    camp, camp_ok, scaffold, fake_agent, Daemon, connect, request,
//    events_json, wait_until, live_session_name, dispatch_one,
//    wait_for_stdout, events_of). See control.rs:10-262. ──
// [reproduce those helpers here]

use serde_json::Value;

/// §5.4 "it can list sessions": `camp sessions --json` returns EVERY live
/// session by name — proving the overseer discovers the fleet over the socket,
/// not by reading `sessions/`.
#[test]
fn camp_sessions_lists_the_whole_fleet_by_name_over_the_socket() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _agent) = scaffold(dir.path(), 10);
    // Two concurrent workers, each lingering in the control loop so both are
    // LIVE at the same time (cardinality ≥ 2 → name-addressing is forced).
    let _d = Daemon::spawn(
        &root,
        &[("FAKE_AGENT_CONTROL_LOOP", "1"), ("FAKE_AGENT_LINGER_ON_EOF", "30")],
    );
    camp_ok(&root, &["sling", "first", "--agent", "dev"]);
    camp_ok(&root, &["sling", "second", "--agent", "dev"]);
    // Wait until the ledger shows two live sessions (both woke).
    wait_until(&root, "two live sessions", |events| {
        events.iter().filter(|e| e["type"] == "session.woke").count() >= 2
    });

    let out = camp_ok(&root, &["sessions", "--json"]);
    let sessions: Vec<Value> = serde_json::from_str(out.trim()).unwrap();
    assert!(sessions.len() >= 2, "expected ≥2 live sessions, got: {out}");
    // Every row is addressed BY NAME and carries no pid field (§4.2 / socket.rs:109).
    for s in &sessions {
        assert!(s["name"].as_str().is_some_and(|n| !n.is_empty()));
        assert!(s.get("pid").is_none(), "SessionInfo must never carry a pid: {s}");
    }
}
```

- [ ] **Step 2: Run to verify it passes (Task 1's client + the merged verb make it green)**

Run: `cargo test -p camp --test overseer camp_sessions_lists_the_whole_fleet -- --nocapture`
Expected: PASS. (If it fails to compile, the harness copy is incomplete — finish reproducing the `control.rs` helpers.)

- [ ] **Step 3: Write the `session.send_turn` + `session.interrupt` tests**

Append:

```rust
/// §5.4 "send them turns": `camp nudge` injects a user turn into the live
/// worker's campd-held stdin (via=stdin) — over the socket.
#[test]
fn camp_nudge_delivers_a_turn_into_the_live_pipe() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _agent) = scaffold(dir.path(), 10);
    let _d = Daemon::spawn(
        &root,
        &[("FAKE_AGENT_CONTROL_LOOP", "1"), ("FAKE_AGENT_LINGER_ON_EOF", "30")],
    );
    let (_bead, session) = dispatch_one(&root);
    // The session must be live with a held pipe before we nudge.
    wait_for_stdout(&root, &session, "\"subtype\":\"init\"");
    let out = camp_ok(&root, &["nudge", &session, "status?"]);
    assert!(out.contains("stdin") || out.contains("held"), "nudge did not use the live pipe: {out}");
    // Durable proof over the socket path: a session.nudged with via=stdin.
    wait_until(&root, "nudged via stdin", |events| {
        events.iter().any(|e| e["type"] == "session.nudged" && e["data"]["via"] == "stdin")
    });
}

/// §5.4 "interrupt them": `camp interrupt` acks a request id and the worker's
/// control_response lands in the ledger — end to end over the socket.
#[test]
fn camp_interrupt_stops_the_turn_over_the_socket() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _agent) = scaffold(dir.path(), 10);
    let _d = Daemon::spawn(&root, &[("FAKE_AGENT_CONTROL_LOOP", "1")]);
    let (_bead, session) = dispatch_one(&root);
    wait_for_stdout(&root, &session, "\"subtype\":\"init\"");
    let out = camp_ok(&root, &["interrupt", &session]);
    assert!(out.contains("interrupt"), "interrupt did not ack: {out}");
    // The worker answers on the read channel → control.responded, verb=session.interrupt.
    wait_until(&root, "control.responded for interrupt", |events| {
        events.iter().any(|e| {
            e["type"] == "control.responded" && e["data"]["verb"] == "session.interrupt"
        })
    });
}
```

- [ ] **Step 4: Run these two tests**

Run: `cargo test -p camp --test overseer camp_nudge camp_interrupt -- --nocapture`
Expected: PASS both.

- [ ] **Step 5: Write the `session.permission_decision` test (BLOCKED → decide → continue)**

Append:

```rust
/// §5.4/§5.3 "answer a permission": a worker asks `can_use_tool`, `camp
/// sessions --json` renders it BLOCKED, `camp decide allow` records+delivers
/// the decision over the socket, and the worker continues. Every step is a
/// socket round-trip; nothing reads the worker's stream file.
#[test]
fn camp_decide_answers_a_blocked_workers_permission_over_the_socket() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _agent) = scaffold(dir.path(), 10);
    let req_id = "cli-overseer";
    let _d = Daemon::spawn(
        &root,
        &[
            ("FAKE_AGENT_CAN_USE_TOOL", "1"),
            ("FAKE_AGENT_CAN_USE_TOOL_REQ", req_id),
        ],
    );
    let (_bead, session) = dispatch_one(&root);
    // campd reads the can_use_tool and marks the session BLOCKED (permission.pending).
    wait_until(&root, "permission.pending", |events| {
        events.iter().any(|e| e["type"] == "permission.pending")
    });
    // The overseer SEES it as BLOCKED over the socket.
    let listed: Vec<Value> =
        serde_json::from_str(camp_ok(&root, &["sessions", "--json"]).trim()).unwrap();
    assert!(
        listed.iter().any(|s| s["name"] == session.as_str() && s["blocked"] == true),
        "the blocked worker must render blocked in sessions.list: {listed:?}"
    );
    // The overseer answers — over the socket.
    let out = camp_ok(&root, &["decide", &session, req_id, "allow"]);
    assert!(out.contains("allow"), "decide did not record allow: {out}");
    wait_until(&root, "permission.decided", |events| {
        events.iter().any(|e| e["type"] == "permission.decided" && e["data"]["decision"] == "allow")
    });
    // And the worker continued (it emits an assistant line after the answer).
    wait_for_stdout(&root, &session, "continued after permission");
}
```

- [ ] **Step 6: Run it**

Run: `cargo test -p camp --test overseer camp_decide_answers -- --nocapture`
Expected: PASS.

- [ ] **Step 7: Write the `session.subscribe` (read-their-streams) test — a bounded child read**

`camp attach` on a live session follows forever, so drive it as a child, read until a known rendered line appears, then kill it — the same bounded-read discipline `control.rs`'s `SubClient` uses. Append:

```rust
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// §5.4 "read their streams": `camp attach` renders the worker's live typed
/// events over `session.subscribe`. Bounded child read: attach, see a rendered
/// line, kill — attach never opens the stream file (its own doc, attach.rs:4).
#[test]
fn camp_attach_streams_a_workers_events_over_the_socket() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _agent) = scaffold(dir.path(), 10);
    let _d = Daemon::spawn(
        &root,
        &[("FAKE_AGENT_CONTROL_LOOP", "1"), ("FAKE_AGENT_LINGER_ON_EOF", "30")],
    );
    let (_bead, session) = dispatch_one(&root);
    wait_for_stdout(&root, &session, "\"subtype\":\"init\"");

    let mut child = Command::new(BIN)
        .env_remove("CAMP_DIR")
        .args(["--camp", root.to_str().unwrap(), "attach", &session])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut saw_stream = false;
    let mut line = String::new();
    while Instant::now() < deadline {
        line.clear();
        if reader.read_line(&mut line).unwrap() == 0 {
            break;
        }
        // attach's own "attached to <session> from byte offset N" hello, then a
        // rendered event line: either proves it is streaming over the socket.
        if line.contains(&session) || line.contains("init") || line.contains("attached") {
            saw_stream = true;
            break;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    assert!(saw_stream, "camp attach produced no rendered stream line");
}
```

- [ ] **Step 8: Run it**

Run: `cargo test -p camp --test overseer camp_attach_streams -- --nocapture`
Expected: PASS.

- [ ] **Step 9: Run the whole overseer file**

Run: `cargo test -p camp --test overseer`
Expected: PASS (all five §5.4-action tests).

- [ ] **Step 10: Commit**

```bash
git add crates/camp/tests/overseer.rs
git commit -m "test(cp-5): overseer performs every §5.4 action against a fake fleet (socket-only)"
```

---

### Task 6: The no-private-paths falsification instrument — necessity, sufficiency, and a static tripwire

The wave's discipline: an instrument that can FALSIFY "the overseer touches only the socket," not one that merely asserts it. Three layers, each RED the moment a client grows a private path:

- **A. Necessity — campd down ⇒ loud failure, never a private-path read.** With the FULL on-disk state present (a live session row in the ledger, its `sessions/<s>.json` stream file, the worker's pid recorded and its process alive) but campd's socket gone, every observe/steer-a-live-worker verb FAILS LOUDLY. A verb that grew a stream-file tail or a pid signal would SUCCEED here → the assertion goes RED. This proves the socket is NECESSARY.
- **B. Sufficiency — private paths unreadable ⇒ every verb still works.** campd UP with a live fake worker, but the worker's `sessions/*.json` stream files and `campd.log` are `chmod 000`. Every overseer verb still succeeds over the socket. campd itself is unaffected — it holds those fds already open (a post-open `chmod` cannot revoke an open descriptor on Unix), so any failure can only come from a CLIENT doing a fresh `open()` of a file it must never touch → RED. This proves the socket is SUFFICIENT.
- **C. Static tripwire.** The pure-client source (`cmd/sessions.rs`, `cmd/interrupt.rs`, `cmd/attach.rs`, `cmd/decide.rs`) must reference `socket::` and NONE of the private-path builders (`sessions_dir`, `stdout_path`, `log_path`, `.join("sessions")`, `/proc`, `libc::kill`, `.pid`). A compile-cheap grep tripwire that goes RED the instant someone wires a private path into an overseer client.

**The `camp nudge` exception (flagged, justified).** `nudge` is the one overseer verb with a non-socket branch: when campd is down or the session has no held pipe it resumes via `claude --resume` (`cmd/nudge.rs:66-106`). That is NOT a §4 private-path violation — it spawns a process keyed on the ledger's recorded `claude_session_id` (a NAME/id, §4.2 rule 1), never tailing a stream file and never signalling a pid. So `nudge` is EXCLUDED from arm A (campd-down legitimately routes to resume) and from arm C (its module is intentionally not socket-pure), but INCLUDED in arm B (campd up, poisoned files ⇒ its live `session.send_turn` path still works). This asymmetry is the honest contract and is stated here rather than discovered later.

**Files:**
- Modify: `crates/camp/tests/overseer.rs` (append arms A, B, and C; reuse the Task 5 harness)

**Interfaces:**
- Consumes: the Task 5 harness; `stdout_path(root, session)` (the exact `sessions/<munge(session)>.json` path, `control.rs:34`); `<root>/campd.log`.
- Produces: the falsifiable instrument.

- [ ] **Step 1: Write arm A — necessity (campd down ⇒ loud fail)**

Append to `overseer.rs`:

```rust
/// FALSIFIER A (§4 necessity): with the ledger, the worker's stream file, and
/// the worker's pid all present on disk but campd's socket GONE, every
/// observe/steer-a-live-worker verb fails LOUDLY. A verb that read the stream
/// file or signalled the pid would SUCCEED here — this assertion is what turns
/// that regression RED. (`camp nudge` is excluded: campd-down legitimately
/// routes to its resume path — see the plan's nudge exception.)
#[test]
fn socket_is_necessary_campd_down_is_a_loud_failure_not_a_private_path_read() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _agent) = scaffold(dir.path(), 10);
    // A worker that OUTLIVES campd, so the pid + stream file are live/present
    // after we kill campd (the tempting private paths are fully populated).
    let session = {
        let d = Daemon::spawn(
            &root,
            &[("FAKE_AGENT_CONTROL_LOOP", "1"), ("FAKE_AGENT_LINGER_ON_EOF", "60")],
        );
        let (_bead, session) = dispatch_one(&root);
        wait_for_stdout(&root, &session, "\"subtype\":\"init\"");
        // The private paths a cheating client would reach for MUST exist now:
        assert!(stdout_path(&root, &session).exists(), "stream file must be present");
        // SIGKILL campd (the harness's crash-only `kill9`, which consumes `d`),
        // leaving the lingering worker + its stream file + the ledger behind.
        d.kill9();
        session
    };
    // With NO socket, each verb must fail loudly — not silently read a file.
    for args in [
        vec!["sessions"],
        vec!["sessions", "--json"],
        vec!["interrupt", &session],
        vec!["decide", &session, "cli-x", "allow"],
        vec!["attach", &session],
    ] {
        let out = camp(&root, &args);
        assert!(
            !out.status.success(),
            "verb `{args:?}` succeeded with campd DOWN — it must reach a live \
             worker only through the socket, never a file or pid"
        );
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("campd") || err.contains("socket"),
            "verb `{args:?}` failed but not with a campd/socket error: {err}"
        );
    }
}
```

Note: reuse the harness's existing `Daemon::kill9(self)` (`control.rs:143-148` — SIGKILL, `wait`, `mem::forget` to avoid a double-kill in `Drop`); it consumes `d`, which is why arm A takes campd down inside a block that returns only `session`. The lingering fake-agent worker is reaped in teardown: it exits on its own `FAKE_AGENT_LINGER_ON_EOF` timeout, and the tempdir drop removes its cwd — mirror the restart tests' cleanup (`control.rs:529-556`), and if flakiness appears add a best-effort `pkill -f "$CAMP_SESSION"` at the end of the test.

- [ ] **Step 2: Run arm A**

Run: `cargo test -p camp --test overseer socket_is_necessary -- --nocapture`
Expected: PASS (every verb fails loudly with campd down).

- [ ] **Step 3: Write arm B — sufficiency (poisoned private paths ⇒ still works)**

Append:

```rust
/// FALSIFIER B (§4 sufficiency): campd UP, but the worker's stream file and
/// campd.log are chmod 000. Every overseer verb still works over the socket.
/// campd is unaffected — it holds those fds already open; only a CLIENT doing a
/// fresh open() of a forbidden file would fail here → RED. (`camp nudge` IS
/// included: its live session.send_turn path must not read those files either.)
#[cfg(unix)]
#[test]
fn socket_is_sufficient_unreadable_private_paths_do_not_stop_any_verb() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let (root, _agent) = scaffold(dir.path(), 10);
    let req_id = "cli-suff";
    let _d = Daemon::spawn(
        &root,
        &[
            ("FAKE_AGENT_CAN_USE_TOOL", "1"),
            ("FAKE_AGENT_CAN_USE_TOOL_REQ", req_id),
            ("FAKE_AGENT_LINGER_ON_EOF", "30"),
        ],
    );
    let (_bead, session) = dispatch_one(&root);
    wait_until(&root, "permission.pending", |events| {
        events.iter().any(|e| e["type"] == "permission.pending")
    });

    // Poison every private path a cheating client might read. campd already
    // holds these fds open, so its own tailing is unaffected.
    let stream = stdout_path(&root, &session);
    let log = root.join("campd.log");
    let saved: Vec<(std::path::PathBuf, std::fs::Permissions)> = [stream.clone(), log.clone()]
        .into_iter()
        .filter(|p| p.exists())
        .map(|p| {
            let perm = std::fs::metadata(&p).unwrap().permissions();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o000)).unwrap();
            (p, perm)
        })
        .collect();

    // Every verb still works — over the socket alone.
    let listed: Vec<Value> =
        serde_json::from_str(camp_ok(&root, &["sessions", "--json"]).trim()).unwrap();
    assert!(listed.iter().any(|s| s["name"] == session.as_str() && s["blocked"] == true));
    camp_ok(&root, &["nudge", &session, "still here?"]); // live send_turn path
    camp_ok(&root, &["decide", &session, req_id, "allow"]);

    // Restore perms so tempdir teardown can clean up.
    for (p, perm) in saved {
        std::fs::set_permissions(&p, perm).unwrap();
    }
}
```

- [ ] **Step 4: Run arm B**

Run: `cargo test -p camp --test overseer socket_is_sufficient -- --nocapture`
Expected: PASS (every verb works despite unreadable stream file + log).

- [ ] **Step 5: Write arm C — the static source tripwire**

Append:

```rust
/// FALSIFIER C: the pure overseer clients must talk to `socket::` and NOTHING
/// that reaches a worker by file or pid. This is the compile-cheap tripwire —
/// it goes RED the instant a private-path builder is imported into a client.
/// (`cmd/nudge.rs` is excluded: its resume path is a documented, name-keyed
/// process spawn, not a stream-file tail or a pid signal — see the plan.)
#[test]
fn pure_overseer_clients_reference_only_the_socket_never_a_private_path() {
    let src = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/cmd");
    let forbidden = [
        "sessions_dir", "stdout_path", "log_path", ".join(\"sessions\")",
        "/proc", "libc::kill", ".pid",
    ];
    for file in ["sessions.rs", "interrupt.rs", "attach.rs", "decide.rs"] {
        let text = std::fs::read_to_string(src.join(file)).unwrap();
        assert!(text.contains("socket::"), "{file} must reach the worker via socket::");
        for needle in forbidden {
            assert!(
                !text.contains(needle),
                "{file} references a PRIVATE PATH `{needle}` — an overseer client \
                 must reach a worker only through the socket (§4)"
            );
        }
    }
}
```

- [ ] **Step 6: Run arm C, then the whole file**

Run: `cargo test -p camp --test overseer`
Expected: PASS (all §5.4-action tests + arms A, B, C).

- [ ] **Step 7: PROVE the instrument can FALSIFY — a temporary mutation goes RED**

Run once, recorded, reverted (never committed). Prove each arm actually catches a violation:

```bash
# Mutate cmd/sessions.rs to read the stream dir instead of the socket, e.g.
# add `let _ = std::fs::read_dir(camp.root.join("sessions"));` at the top of run().
cargo test -p camp --test overseer socket_is_necessary   # EXPECT: FAIL (verb succeeds with campd down)
cargo test -p camp --test overseer pure_overseer_clients  # EXPECT: FAIL (.join("sessions"))
git checkout crates/camp/src/cmd/sessions.rs              # revert
```

Record in the PR that the mutation flipped arms A and C RED and was reverted. (Arm B is falsified by any client that reads a poisoned file; the same mutation, made to read the stream FILE, also flips B.)

- [ ] **Step 8: Commit**

```bash
git add crates/camp/tests/overseer.rs
git commit -m "test(cp-5): no-private-paths instrument — socket is necessary AND sufficient (§4)"
```

---

### Task 7: Full-suite green + verification-before-completion

**Files:** none (verification only).

- [ ] **Step 1: Run the full camp test suite**

Run: `cargo test -p camp`
Expected: PASS, including the pre-existing `control.rs`, `plugin_operator_skill.rs`, `plugin_policy.rs`, and the new `overseer.rs`.

- [ ] **Step 2: Clippy at the repo's bar (no unwrap/expect/panic in library code, no unsafe)**

Run: `cargo clippy -p camp --all-targets -- -D warnings`
Expected: clean. (`cmd/sessions.rs` and `cmd/interrupt.rs` contain no `unwrap`/`expect`/`panic` in non-test code; the test module carries the standard `#![allow(...)]`.)

- [ ] **Step 3: Confirm the plugin tree is still agent-definition-free**

Run: `cargo test -p camp --test plugin_policy && git status --porcelain plugin/`
Expected: PASS; the only `plugin/` change in the branch is `skills/operator/SKILL.md`.

- [ ] **Step 4: Push the branch**

```bash
git push -u origin cp-5-overseer
```

---

## Self-Review

**1. Spec coverage.**
- §5.4 "list sessions" → Task 1 (`camp sessions` / `sessions.list`) + Task 5 Step 1. ✓
- §5.4 "read their streams" → Task 5 Step 7 (`camp attach` / `session.subscribe`). ✓
- §5.4 "send them turns" → Task 5 Step 3 (`camp nudge` / `session.send_turn`). ✓
- §5.4 "interrupt them" → Task 2 + Task 5 Step 3 (`camp interrupt` / `session.interrupt`). ✓
- §5.3 "answer a permission" → Task 5 Step 5 (`camp decide` / `session.permission_decision`). ✓
- §4 no-private-paths, PROVABLY (falsifiable) → Task 6 arms A/B/C. ✓
- master §11 zero-agent-definitions, with a RED proof → Task 4. ✓
- "the overseer becomes a client, not a special case; needs no new machinery" → Task 3 skill rewrite; the two new CLIs add zero protocol/daemon machinery (flagged under Contract Deviations). ✓
- Exit criterion "every §5.4 action against a FAKE fleet through the socket alone; CI green" → Task 5 (fake-fleet, socket-only) + Task 7 (CI green). ✓

**2. Placeholder scan.** No "TBD"/"add error handling"/"similar to Task N". Every code step shows complete code; the one deliberate deferral (the `control.rs` harness helpers reproduced into `overseer.rs`) is a verbatim copy of pinned, existing code with an exact citation (`control.rs:10-262`), not an invention — this keeps the plan DRY without hiding logic. The `Daemon::kill_hard` helper is specified with its model (`control.rs:529-556`).

**3. Type consistency.** `SessionInfo { name, agent, rig, bead, state, last_activity, blocked }` matches `socket.rs:112-130` exactly (the unit-test constructor in Task 1 fills all seven fields). `Request::SessionsList`, `Request::SessionInterrupt { session }`, `Response::SessionsList { sessions, .. }`, `Response::Interrupt { request_id, .. }` match `socket.rs`. `socket::require` signature matches `socket.rs:436`. `cmd::sessions::run(&CampDir, bool)` and `cmd::interrupt::run(&CampDir, String)` are consistent between their modules and the `main.rs` dispatch arms.

## Contract Deviations (additive only)

1. **Two new one-shot CLI clients — `camp sessions` and `camp interrupt`.** §5.4 says the overseer "needs no new machinery." That is true of the PROTOCOL and the daemon: cp-5 adds no verb, no `Request`/`Response` field, no handler. It adds two thin client-side CLI surfaces because the merged CLI only exposed the streaming/human forms of these two verbs (`camp watch` blocks; `camp attach`'s `/interrupt` is inside an interactive loop; `camp top` is aggregate counts), and an *agent* overseer needs a one-shot snapshot to learn session names and a one-shot stop. This is the exact precedent cp-3 set with `camp decide` (a one-shot pure client over `session.permission_decision`). Both are pure `socket::require` round-trips. **Additive, no removal, no protocol change.**
2. **`camp nudge`'s resume fallback is NOT a §4 private-path violation.** `nudge` resumes via `claude --resume` keyed on the ledger's recorded `claude_session_id` when campd is down or there is no held pipe (`cmd/nudge.rs:66-106`). It never tails a stream file and never signals a pid — it addresses by name/id (§4.2 rule 1). The falsification instrument therefore scopes `nudge` OUT of arm A (necessity) and arm C (static purity) and IN to arm B (sufficiency). Stated explicitly so a reviewer does not read the exclusion as a hole.
3. **Policy test strengthened, not just reused.** Task 4 adds an `agent.toml`-anywhere check to `plugin_policy.rs` beyond the existing `agents/`-directory check, because compat §5.1 defines an agent as a directory-with-`agent.toml`, and a bare `agent.toml` outside a literal `agents/` dir would otherwise pass. Additive to the existing guard.

## Execution Handoff

This document is the deliverable of a planning-only session. A fresh implementer session executes it after the plan gate approves it. Recommended execution: **superpowers:subagent-driven-development** (a fresh subagent per task, two-stage review between tasks), because Tasks 1–2 (client code), 3 (skill+pin), 4 (policy), and 5–6 (the falsifiable instrument) each carry an independent test cycle and a meaningful review boundary.

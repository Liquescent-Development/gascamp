# cp-4 — `camp attach`, the per-agent view — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `camp attach <session>` — the per-agent view: a client that opens `session.subscribe`, renders one worker's typed event stream live (tool calls with inputs, tool results, assistant text, token usage), filters it, replays a finished session from the durable byte history, and can send a turn or interrupt — plus the spawn-side `--include-partial-messages` gate that lets a live attach see token deltas without autonomous dispatch gaining them.

**Architecture:** Everything the per-agent view needs on the DAEMON side already merged in cp-1: `session.subscribe` is a long-lived server-push MODE that delivers a worker's stdout stream file as `event`/`skipped`/`end` frames from a BYTE-OFFSET cursor (§9), and `session.send_turn` / `session.interrupt` are the two steering verbs. So cp-4 is overwhelmingly a **stateless client** (§4.2): `camp attach` connects, subscribes from a cursor, decodes each frame, renders the inner typed event, and prints it — replay and live-follow are the SAME subscribe (cursor 0 replays history then follows; on a finished session it terminates at `end`). The ONE daemon/core change is `build_spec` gaining a spawn-time `--include-partial-messages` flag on the HeldStream arm, gated OFF by default so autonomous dispatch never emits token deltas (§2.2).

**Tech Stack:** Rust, std unix sockets (the client speaks the newline-JSON wire directly, like `camp watch`/`camp nudge`), serde/serde_json, clap CLI. No new dependencies.

---

## ⚠ SCOPE DECISION — replay source (PENDING OPERATOR CONFIRMATION)

**This block is load-bearing; the plan gate and the operator must both see it.** There is a genuine SPEC-SCOPE tension between §5.2 and §9, and narrowing it is a decision the OPERATOR owns. The lead has surfaced it to the operator; this plan proceeds on the working assumption below and can be redirected with a single edit.

- **§5.2** says the per-agent view can *"replay — scrub back through a finished session; the transcript is durable."*
- **§9** says *"`session.subscribe` cursors are byte offsets into the STREAM file … A cursor into a reaped (disposed) stream is an explicit error."*

These name **different files**: the stdout stream file `sessions/<munge>.json` (byte-cursored, subscribe-served, **unlinked at reap** — `read_channel.rs:566`) versus the `.jsonl` transcript (claude's own conversation log, persists forever, recorded in `session.woke`; **no code reads it today**).

**WORKING ASSUMPTION (pending operator confirmation):** cp-4 delivers **§9 stream-file replay of a RETAINED session** — `session.subscribe` from cursor 0 replays the full byte history and terminates with the `end{reason:"stopped"}` frame (cp-1's `closing`/`Disposed` machinery keeps the fd alive through disposal, so an attached-from-0 subscriber finishes the history). A **REAPED** (long-gone, disposed) session's subscribe is the **explicit §9 error**, which the client surfaces. **§5.2's replay-from-the-durable-`.jsonl`-transcript — a net-new socket verb plus a claude-transcript format adapter — is DEFERRED to a follow-up phase.**

**Why defensible:** it is the exact §9 mechanism; the transcript path has zero existing code and is a much larger, separable capability; and it satisfies the exit criterion "replay of a finished session" via the cursor-0 replay + terminal `end` frame (Task 6). **If the operator wants genuine transcript replay built now, this becomes a materially larger plan (new verb + format adapter) and must be re-scoped** — the plan is built so that a transcript source drops in later behind the SAME client renderer.

---

## ⚠ SCOPE DECISION — `--include-partial-messages` gate mechanism (design call; endorsed by lead)

The flag is fixed at SPAWN time; ALL real dispatch is `StdinMode::HeldStream` (`dispatch.rs:657`, *"ALL campd dispatch spawns hold the stream stdin"*); there is no per-agent opt-in field today. So *"attach needs it, autonomous dispatch must NOT gain it"* (§2.2) is implementable only as an opt-in that **DEFAULTS OFF**.

**DECISION: a per-agent opt-in** — `AgentDef.partial_messages: bool` (default false, from `agent.toml`), threaded into `build_spec`, appended to the HeldStream arm only. **Rationale:** it MIRRORS cp-3's per-agent `--permission-prompt-tool stdio` (§5.3.1), so the two siblings extend the `spawn.rs` per-agent-flag surface with ONE coordinated pattern rather than two mechanisms — consistency that matters because cp-4 and cp-3 share that argv region.

**Rejected alternative — a camp-wide `[dispatch] include_partial_messages` config toggle.** It is smaller and less-contended (`config.rs`, not the guarded `AgentDef` surface), and "default off" would still satisfy the pin. Rejected because it is all-or-nothing: flipping it on gives token deltas to EVERY autonomous worker at once (stream bloat + the "never goes quiet" hazard at `control.rs:318`), and it does not mirror cp-3's per-agent shape. The reviewer can weigh this; Task 5 is structured so the load-bearing core (the `build_spec` bool + the default-off pin) is INDEPENDENT of the opt-in SOURCE, so a swap to the config toggle is localized to one step.

**cp-3 coordination:** `AgentDef.partial_messages` (cp-4) and cp-3's new per-agent permission field are BOTH new optional guarded-surface fields defaulting off, landing in the SAME `AgentDef`/`agent.toml` surface AND the same `build_spec` HeldStream arm. They will need an **additive rebase between the two implementations** (worktree isolation handles planning; the lead calls the rebase after cp-3 merges).

**The three test-pinnable requirements (the spine of Task 5), each with its named mutation:**
1. **Default autonomous dispatch argv contains NO `--include-partial-messages`** (the §2.2 "must NOT gain it" half). *Mutation caught:* a regression that appends the flag unconditionally — the flag leaks into unattended dispatch and this pin goes RED.
2. **An opted-in spawn's argv DOES carry it**, and only on the HeldStream (stream-json) arm. *Mutation caught:* the gate wired to the wrong arm, or dropped entirely.
3. **Attach's live view is built on the COMPLETE events** (tool calls/inputs, results, assistant text, token usage) that are present WITHOUT the flag — so attach+detach and replay work regardless of the flag; the flag only adds the token-by-token DELTA enrichment. **The exit criteria do NOT depend on the flag** (Task 6 never sets it).

---

## Global Constraints

Copied verbatim from AGENTS.md invariants and the kickoff; every task's requirements implicitly include these.

- **Idle is free.** No ticks, no polling loops. `camp attach` is push-driven: it BLOCKS on the socket between frames — zero polling. A quiet attached session costs zero wakeups (it inherits cp-1's idle property; no new standing cost).
- **Fail fast.** No fallbacks, no silenced errors, no placeholders. No panics in library code — clippy `unwrap_used`/`expect_used`/`panic` are DENIED outside `#[cfg(test)]`; `unsafe_code` forbidden. Every error surfaces to the caller. A malformed frame from campd is a loud client error, not a swallowed line.
- **Nothing hidden.** A normal detach is NOT a fault and appends NO event (cp-1 already guarantees this: an EPIPE/ECONNRESET on a subscriber socket is `FlushStep::Gone`, silent — `control.rs:3833-3835`). cp-4 must not add an event on detach.
- **Sessions are addressed by name, never by pid or file path** (§4.2). `camp attach` takes a session NAME; it never opens a session file, never learns a pid. It reaches the worker ONLY through the socket (§4). The stream file path lives in campd; the client never derives or opens it.
- **campd owns the truth; clients are stateless renderers** (§4.2). `camp attach` renders what campd sends over `session.subscribe`; it never tails a file or reads the ledger/transcript directly.
- **The transport is swappable; the protocol is not** (§4.2). cp-4 adds NO new socket verb and NO new wire shape — it consumes cp-1's `session.subscribe` / `session.send_turn` / `session.interrupt` verbs verbatim. The only new bytes on any wire are the argv `--include-partial-messages` flag between campd and the worker (Task 5).
- **cp-4 introduces NO new `EventType`.** The per-agent view is transport + a spawn flag; it emits no durable event. So `crates/camp-core/src/event.rs`, `vocab.rs`, `ledger/fold.rs` are NOT modified by this plan.
- **DEFERRED — the permission-answer action (§5.2 "answer a permission request").** cp-3 owns the `session.permission_decision` verb, which does not exist while cp-4 runs parallel to cp-3. Build the view so the answer action DROPS IN after cp-3 merges; do NOT build the permission path here (mirrors cp-2's BLOCKED column). Concretely: the renderer surfaces a `can_use_tool` event as a visible "waiting on your decision" line, but cp-4 wires NO decision-sending action.
- **Guaranteed-contention files must stay ADDITIVE** (cp-3 + compat-4 run in parallel): `crates/camp/src/main.rs`, `crates/camp-core/src/event.rs`, `crates/camp-core/src/vocab.rs`, `crates/camp-core/src/ledger/fold.rs`, `Cargo.toml`, `Cargo.lock`. cp-4 touches ONLY `main.rs` among these (one additive `Attach` command variant + `pub mod attach;` + a dispatch arm) and adds NO dependency (`Cargo.toml`/`Cargo.lock` untouched). **`spawn.rs` and `AgentDef` are SHARED-WITH-cp-3 surfaces** — keep the cp-4 changes tightly scoped (the HeldStream arm; one new optional `AgentDef` field); an additive rebase reconciles the two siblings.
- **Gates green before any push:** `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace`. Perf gate (`make perf`) is LOCAL-ONLY; cp-4 adds no standing cost, so no new perf arm is required (Task 7 notes the argument).
- **Branch:** `cp-4-camp-attach`. Never commit to main. No co-author lines. After any merge to main, rebase onto main and re-run the gates before continuing. After cp-3 merges, the lead will call for a rebase; reconcile the `spawn.rs` HeldStream arm and the `AgentDef` surface with cp-3's flag then.

---

## Scoping decisions (read before Task 1)

Decisions this plan makes where the spec's end-state is richer than cp-4's slice. Each is documented so the implementer does not "fix" a deliberate boundary. (The two ⚠ decisions above are the load-bearing ones; these are the smaller ones.)

1. **Live-follow and replay are the SAME subscribe.** `session.subscribe` with `cursor: Some(0)` delivers the full byte history and then FOLLOWS live (§4.1 "a late joiner gets history, then follows"); on a finished session the same subscription terminates at `end`. So `camp attach <session>` defaults to `cursor: Some(0)` (history-then-follow), `--tail` maps to `cursor: None` (live only), and `--from <offset>` resumes from a durable §9 cursor. There is no separate "replay mode".
2. **The steering surface is line-oriented, not keystroke-level** (§6: "you send turns and decisions, not keypresses"). While attached, a plain input line is sent as a turn (`session.send_turn`), `/interrupt` interrupts (`session.interrupt`), `/q` (or EOF) detaches. The line→action mapping is a pure, unit-tested function; the send reuses the proven verbs on separate one-shot connections. There is no TUI, no cursor addressing.
3. **The permission-answer action is rendered-but-not-wired** (DEFERRED to cp-3). A `can_use_tool` event renders as a visible "BLOCKED — waiting on your decision (cp-3)" line; cp-4 sends no decision. When cp-3's `session.permission_decision` verb lands, a `/allow` / `/deny` action drops into the same line loop.
4. **Filter is client-side and coarse** (§5.2 "show me only the Edits / only the failures"). cp-4 ships `--only <all|text|tools|edits|failures>`, a pure predicate over the parsed event. Finer filters are additive later.

---

## File structure

- **Create `crates/camp/src/cmd/attach.rs`** — the whole client. Pure, unit-tested logic: `render_event` (a stream-json event → rendered lines with a filterable kind), `AttachFilter` (the `--only` predicate), `StreamFrame`/`decode` (a `session.subscribe` wire line → a typed frame), `parse_action` (an input line → an `Action`), `render_frame` (frame → filtered print lines), `subscribe_cursor` (the cursor policy). Thin IO glue: `run` (connect, subscribe, spawn the stream-reader thread, run the stdin action loop, detach).
- **Modify `crates/camp/src/daemon/spawn.rs`** — `build_spec` gains `include_partial_messages: bool`; the HeldStream arm appends `--include-partial-messages` when true. The two argv pin tests assert the default-off asymmetry. SHARED WITH cp-3 — scoped to the HeldStream arm.
- **Modify `crates/camp-core/src/pack.rs`** — `AgentDef.partial_messages: bool` + `resolve_agent_def` / `parse_agent_dir`. SHARED-SURFACE with cp-3 (both add one optional guarded-surface field).
- **Modify `crates/camp/src/daemon/dispatch.rs`** — the single `build_spec` call site (`dispatch.rs:657`) passes `agent.partial_messages`.
- **Modify `crates/camp/src/main.rs`** — additive: `pub mod attach;`, the `Attach { session, only, tail, from }` command variant, its dispatch arm.
- **Modify `crates/camp/tests/control.rs`** — the two exit-criteria e2e tests, reusing the existing `Daemon`/`scaffold`/`dispatch_one`/`connect`/`request` harness.

---

## Interfaces the client CONSUMES from merged code (do not re-derive)

- Wire (from `crates/camp/src/daemon/socket.rs`): `Request::SessionSubscribe { session: String, cursor: Option<u64> }`, `Request::SessionSendTurn { session, text }`, `Request::SessionInterrupt { session }`; `Response::Subscribed { ok, v, subscription, cursor }`, `Response::SendTurn { ok, via }`, `Response::Interrupt { ok, request_id }`, `Response::Error { ok, error }`; `socket::{request, request_if_up, require, REQUEST_TIMEOUT}`; `CampDir::socket_path`.
- `session.subscribe` frames on the wire (from `control.rs`, byte-pinned there): each is a newline-terminated JSON object with a `"frame"` tag:
  - `{"frame":"event","session":"…","offset":N,"event":<raw stream-json object, spliced verbatim>}`
  - `{"frame":"skipped","session":"…","offset":N,"bytes":B,"reason":"over_cap"|"not_a_json_object"}`
  - `{"frame":"end","session":"…","offset":N,"reason":"stopped"|"crashed"}`
  - `offset` is the byte offset of the START OF THE NEXT LINE — the durable §9 resume cursor (`control.rs:3613`).
- Client precedent to mirror for the IO shape: `crates/camp/src/cmd/watch.rs` (connect → write request → read hello → long-lived read loop, `set_read_timeout(None)` after the hello) and `crates/camp/src/cmd/nudge.rs:40-61` (`socket::request_if_up` with `Request::SessionSendTurn`).

---

## Task 1: The typed-event renderer (pure)

The heart of the per-agent view: turn one worker stream-json event (the inner `event` value of an `event` frame) into rendered lines, each tagged with a KIND so the filter can select. Pure and exhaustively unit-tested; leniently handles unknown kinds (never panics on a shape it does not recognize — including partial-message deltas).

**Files:**
- Create: `crates/camp/src/cmd/attach.rs`
- Test: `crates/camp/src/cmd/attach.rs` (its `#[cfg(test)]` module)

**Interfaces:**
- Produces:
  - `#[derive(Debug, Clone, PartialEq)] enum EventKind { Text, ToolUse { tool: String }, ToolResult { is_error: bool }, Result, Permission, System, Other }`
  - `#[derive(Debug, Clone, PartialEq)] struct Rendered { pub kind: EventKind, pub line: String }`
  - `fn render_event(ev: &serde_json::Value) -> Vec<Rendered>` — a single stream-json event yields zero or more rendered lines (an assistant message with two content blocks yields two).
  - `fn tool_summary(name: &str, input: &serde_json::Value) -> String` (helper).

- [ ] **Step 1: Create the file skeleton and the failing renderer tests**

Create `crates/camp/src/cmd/attach.rs` with ONLY the test module first (the impl comes in Step 3). Note the module is not declared in `main.rs` yet — Task 4 declares it. To compile the test now, add `pub mod attach;` to `main.rs`'s `pub mod` block (Task 4 keeps it); either way the assertions below are the contract.

```rust
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn renders_assistant_text() {
        let ev = json!({
            "type": "assistant",
            "message": {"role": "assistant", "content": [{"type": "text", "text": "on it"}]}
        });
        let r = render_event(&ev);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].kind, EventKind::Text);
        assert!(r[0].line.contains("on it"), "{:?}", r[0]);
    }

    #[test]
    fn renders_tool_use_with_a_salient_input_summary() {
        let ev = json!({
            "type": "assistant",
            "message": {"role": "assistant", "content": [
                {"type": "tool_use", "id": "t1", "name": "Edit", "input": {"file_path": "src/lib.rs"}}
            ]}
        });
        let r = render_event(&ev);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].kind, EventKind::ToolUse { tool: "Edit".into() });
        assert!(r[0].line.contains("Edit") && r[0].line.contains("src/lib.rs"), "{:?}", r[0]);
    }

    #[test]
    fn renders_a_bash_tool_use_by_its_command() {
        let ev = json!({
            "type": "assistant",
            "message": {"role": "assistant", "content": [
                {"type": "tool_use", "id": "t2", "name": "Bash", "input": {"command": "cargo publish"}}
            ]}
        });
        let r = render_event(&ev);
        assert_eq!(r[0].kind, EventKind::ToolUse { tool: "Bash".into() });
        assert!(r[0].line.contains("cargo publish"), "{:?}", r[0]);
    }

    #[test]
    fn an_assistant_message_with_two_blocks_yields_two_lines() {
        let ev = json!({
            "type": "assistant",
            "message": {"role": "assistant", "content": [
                {"type": "text", "text": "let me look"},
                {"type": "tool_use", "id": "t3", "name": "Read", "input": {"file_path": "README.md"}}
            ]}
        });
        let r = render_event(&ev);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].kind, EventKind::Text);
        assert_eq!(r[1].kind, EventKind::ToolUse { tool: "Read".into() });
    }

    #[test]
    fn renders_tool_result_success_and_error() {
        let ok = json!({
            "type": "user",
            "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "t1", "content": "done", "is_error": false}
            ]}
        });
        let err = json!({
            "type": "user",
            "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "t2", "content": "boom", "is_error": true}
            ]}
        });
        assert_eq!(render_event(&ok)[0].kind, EventKind::ToolResult { is_error: false });
        assert_eq!(render_event(&err)[0].kind, EventKind::ToolResult { is_error: true });
        assert!(render_event(&err)[0].line.to_lowercase().contains("error"), "error is visible");
    }

    #[test]
    fn renders_the_result_event_with_usage() {
        let ev = json!({
            "type": "result", "subtype": "success",
            "usage": {"input_tokens": 10, "output_tokens": 20}, "total_cost_usd": 0.01
        });
        let r = render_event(&ev);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].kind, EventKind::Result);
        assert!(r[0].line.contains("20") || r[0].line.to_lowercase().contains("token"), "{:?}", r[0]);
    }

    #[test]
    fn a_can_use_tool_control_request_renders_as_a_visible_permission_line() {
        // cp-3 owns the ANSWER; cp-4 makes the QUESTION impossible to miss.
        let ev = json!({
            "type": "control_request", "request_id": "r1",
            "request": {"subtype": "can_use_tool", "tool_name": "Bash", "input": {"command": "rm -rf /"}}
        });
        let r = render_event(&ev);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].kind, EventKind::Permission);
        assert!(r[0].line.to_uppercase().contains("BLOCKED"), "{:?}", r[0]);
    }

    #[test]
    fn an_unknown_event_kind_renders_leniently_and_never_panics() {
        // Partial-message deltas and any future shape must not crash the view.
        let delta = json!({"type": "stream_event", "event": {"type": "content_block_delta",
            "delta": {"type": "text_delta", "text": "par"}}});
        let weird = json!({"type": "something_new", "x": 1});
        let _ = render_event(&delta); // must not panic
        let r = render_event(&weird);
        // Other is allowed to be empty OR a compact fallback — but never a panic.
        assert!(r.iter().all(|x| x.kind == EventKind::Other), "{r:?}");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p camp --lib -- cmd::attach` (after adding `pub mod attach;`)
Expected: FAIL — `render_event`, `EventKind`, `Rendered` undefined.

- [ ] **Step 3: Implement the renderer**

Add above the test module in `crates/camp/src/cmd/attach.rs`:

```rust
//! `camp attach <session>` (control-plane spec §5.2): the per-agent view. A
//! STATELESS RENDERER (§4.2): it opens a `session.subscribe` stream and renders
//! one worker's typed events live — tool calls with inputs, tool results,
//! assistant text, token usage. It never opens a session file, never learns a
//! pid; it reaches the worker only through the socket. Replay and live-follow are
//! the SAME subscribe (cursor 0 replays history then follows; a finished session
//! ends). From here you send a turn or interrupt; answering a permission request
//! drops in when cp-3's verb lands.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

use anyhow::{Result, bail};
use serde::Deserialize;

use crate::campdir::CampDir;
use crate::daemon::socket::{self, Request, Response};

/// The KIND of a rendered line — what the `--only` filter selects on.
#[derive(Debug, Clone, PartialEq)]
pub enum EventKind {
    Text,
    ToolUse { tool: String },
    ToolResult { is_error: bool },
    Result,
    /// A `can_use_tool` control request — a question addressed to the operator.
    /// cp-4 renders it; cp-3 owns answering it.
    Permission,
    System,
    Other,
}

/// One renderable line, tagged with its kind.
#[derive(Debug, Clone, PartialEq)]
pub struct Rendered {
    pub kind: EventKind,
    pub line: String,
}

/// The salient input field for common tools, so a tool call reads like the
/// fleet view's `Edit(src/lib.rs)` / `Bash(cargo publish)`.
fn tool_summary(name: &str, input: &serde_json::Value) -> String {
    let key = match name {
        "Edit" | "Write" | "Read" | "MultiEdit" | "NotebookEdit" => "file_path",
        "Bash" => "command",
        "Grep" | "Glob" => "pattern",
        _ => "",
    };
    match input.get(key).and_then(|v| v.as_str()) {
        Some(v) => format!("{name}({v})"),
        None => name.to_owned(),
    }
}

/// Render ONE stream-json event into zero or more lines. Lenient: an unrecognized
/// shape yields `Other` (or nothing), never a panic — partial-message deltas and
/// any future event kind flow through here untyped rather than crashing the view.
pub fn render_event(ev: &serde_json::Value) -> Vec<Rendered> {
    let ty = ev.get("type").and_then(|v| v.as_str()).unwrap_or("");
    match ty {
        "assistant" | "user" => {
            let content = ev
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_array());
            let Some(blocks) = content else {
                return vec![];
            };
            let mut out = Vec::new();
            for b in blocks {
                let bty = b.get("type").and_then(|v| v.as_str()).unwrap_or("");
                match bty {
                    "text" => {
                        if let Some(t) = b.get("text").and_then(|v| v.as_str()) {
                            let t = t.trim();
                            if !t.is_empty() {
                                out.push(Rendered { kind: EventKind::Text, line: t.to_owned() });
                            }
                        }
                    }
                    "tool_use" => {
                        let name = b.get("name").and_then(|v| v.as_str()).unwrap_or("tool");
                        let empty = serde_json::Value::Null;
                        let input = b.get("input").unwrap_or(&empty);
                        out.push(Rendered {
                            kind: EventKind::ToolUse { tool: name.to_owned() },
                            line: format!("  → {}", tool_summary(name, input)),
                        });
                    }
                    "tool_result" => {
                        let is_error = b.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false);
                        let body = b
                            .get("content")
                            .and_then(|v| v.as_str())
                            .map(|s| s.lines().next().unwrap_or("").to_owned())
                            .unwrap_or_default();
                        let tag = if is_error { "  x error" } else { "  ok" };
                        out.push(Rendered {
                            kind: EventKind::ToolResult { is_error },
                            line: format!("{tag} {body}"),
                        });
                    }
                    _ => out.push(Rendered { kind: EventKind::Other, line: String::new() }),
                }
            }
            out
        }
        "result" => {
            let out_toks = ev
                .get("usage")
                .and_then(|u| u.get("output_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let cost = ev.get("total_cost_usd").and_then(|v| v.as_f64()).unwrap_or(0.0);
            vec![Rendered {
                kind: EventKind::Result,
                line: format!("-- result: {out_toks} output tokens, ${cost:.4} --"),
            }]
        }
        "control_request" => {
            let sub = ev
                .get("request")
                .and_then(|r| r.get("subtype"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if sub == "can_use_tool" {
                let tool = ev
                    .get("request")
                    .and_then(|r| r.get("tool_name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("a tool");
                vec![Rendered {
                    kind: EventKind::Permission,
                    line: format!("  !! BLOCKED -- {tool} needs your decision (answer lands when cp-3 ships)"),
                }]
            } else {
                vec![Rendered { kind: EventKind::Other, line: String::new() }]
            }
        }
        "system" => vec![Rendered { kind: EventKind::System, line: String::new() }],
        _ => vec![Rendered { kind: EventKind::Other, line: String::new() }],
    }
}
```

- [ ] **Step 4: Run the renderer tests**

Run: `cargo test -p camp --lib -- cmd::attach::tests`
Expected: PASS (all eight render tests).
Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/camp/src/cmd/attach.rs crates/camp/src/main.rs
git commit -m "feat(cli): the attach event renderer -- typed stream-json to rendered lines (cp-4)"
```

---

## Task 2: The `--only` filter (pure)

A coarse client-side filter (§5.2 "only the Edits", "only the failures"). A pure predicate over `Rendered.kind`.

**Files:**
- Modify: `crates/camp/src/cmd/attach.rs`
- Test: same file (test module)

**Interfaces:**
- Consumes: `EventKind`, `Rendered` (Task 1).
- Produces:
  - `#[derive(Debug, Clone, Copy, PartialEq)] enum AttachFilter { All, Text, Tools, Edits, Failures }`
  - `impl AttachFilter { fn parse(s: &str) -> anyhow::Result<AttachFilter>; fn matches(&self, r: &Rendered) -> bool }`
  - `const EDIT_TOOLS: &[&str]` (the tools `Edits` selects).

- [ ] **Step 1: Write the failing filter tests**

```rust
#[test]
fn filter_all_admits_everything() {
    let f = AttachFilter::All;
    assert!(f.matches(&Rendered { kind: EventKind::Text, line: "x".into() }));
    assert!(f.matches(&Rendered { kind: EventKind::Result, line: "x".into() }));
}

#[test]
fn filter_edits_admits_only_edit_family_tool_uses() {
    let f = AttachFilter::Edits;
    assert!(f.matches(&Rendered { kind: EventKind::ToolUse { tool: "Edit".into() }, line: "x".into() }));
    assert!(f.matches(&Rendered { kind: EventKind::ToolUse { tool: "Write".into() }, line: "x".into() }));
    assert!(!f.matches(&Rendered { kind: EventKind::ToolUse { tool: "Bash".into() }, line: "x".into() }));
    assert!(!f.matches(&Rendered { kind: EventKind::Text, line: "x".into() }));
}

#[test]
fn filter_failures_admits_error_results_only() {
    let f = AttachFilter::Failures;
    assert!(f.matches(&Rendered { kind: EventKind::ToolResult { is_error: true }, line: "x".into() }));
    assert!(!f.matches(&Rendered { kind: EventKind::ToolResult { is_error: false }, line: "x".into() }));
    assert!(!f.matches(&Rendered { kind: EventKind::Text, line: "x".into() }));
}

#[test]
fn filter_tools_admits_tool_uses_and_results() {
    let f = AttachFilter::Tools;
    assert!(f.matches(&Rendered { kind: EventKind::ToolUse { tool: "Bash".into() }, line: "x".into() }));
    assert!(f.matches(&Rendered { kind: EventKind::ToolResult { is_error: false }, line: "x".into() }));
    assert!(!f.matches(&Rendered { kind: EventKind::Text, line: "x".into() }));
}

#[test]
fn filter_parse_rejects_an_unknown_name() {
    assert!(AttachFilter::parse("all").is_ok());
    assert!(AttachFilter::parse("edits").is_ok());
    assert!(AttachFilter::parse("nonsense").is_err());
}

#[test]
fn a_permission_line_survives_every_filter() {
    // BLOCKED must be impossible to miss -- no filter may hide it (§5.3 spirit).
    let p = Rendered { kind: EventKind::Permission, line: "!! BLOCKED".into() };
    for f in [AttachFilter::All, AttachFilter::Text, AttachFilter::Tools, AttachFilter::Edits, AttachFilter::Failures] {
        assert!(f.matches(&p), "{f:?} hid a permission line");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p camp --lib -- cmd::attach::tests::filter`
Expected: FAIL — `AttachFilter` undefined.

- [ ] **Step 3: Implement the filter**

Add to `crates/camp/src/cmd/attach.rs` (above the test module):

```rust
/// Tools the `edits` filter selects -- the file-mutating family.
pub const EDIT_TOOLS: &[&str] = &["Edit", "Write", "MultiEdit", "NotebookEdit"];

/// The `--only` filter (§5.2). Coarse by design; finer filters are additive.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AttachFilter {
    All,
    Text,
    Tools,
    Edits,
    Failures,
}

impl AttachFilter {
    pub fn parse(s: &str) -> Result<AttachFilter> {
        match s {
            "all" => Ok(AttachFilter::All),
            "text" => Ok(AttachFilter::Text),
            "tools" => Ok(AttachFilter::Tools),
            "edits" => Ok(AttachFilter::Edits),
            "failures" => Ok(AttachFilter::Failures),
            other => bail!("unknown --only filter {other:?}: expected all|text|tools|edits|failures"),
        }
    }

    /// Does this line pass? A `Permission` line ALWAYS passes -- a question
    /// addressed to the operator must never be filtered away.
    pub fn matches(&self, r: &Rendered) -> bool {
        if r.kind == EventKind::Permission {
            return true;
        }
        match self {
            AttachFilter::All => true,
            AttachFilter::Text => r.kind == EventKind::Text,
            AttachFilter::Tools => matches!(r.kind, EventKind::ToolUse { .. } | EventKind::ToolResult { .. }),
            AttachFilter::Edits => matches!(&r.kind, EventKind::ToolUse { tool } if EDIT_TOOLS.contains(&tool.as_str())),
            AttachFilter::Failures => matches!(r.kind, EventKind::ToolResult { is_error: true })
                || r.kind == EventKind::Result && r.line.to_lowercase().contains("error"),
        }
    }
}
```

- [ ] **Step 4: Run the filter tests + gates**

Run: `cargo test -p camp --lib -- cmd::attach::tests`
Expected: PASS.
Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/camp/src/cmd/attach.rs
git commit -m "feat(cli): the attach --only filter -- coarse client-side selection, permission lines always shown (cp-4)"
```

---

## Task 3: The frame decoder + action parser (pure)

Decode a `session.subscribe` wire line into a typed `StreamFrame`, and map an input line to a steering `Action`. Both pure; both unit-tested. The decoder is LENIENT (an unknown `frame` is ignored, never a crash — mirroring `watch.rs`'s `Frame`).

**Files:**
- Modify: `crates/camp/src/cmd/attach.rs`
- Test: same file

**Interfaces:**
- Produces:
  - `#[derive(Debug, Deserialize)] #[serde(tag = "frame", rename_all = "snake_case")] enum StreamFrame { Event { offset: u64, event: serde_json::Value }, Skipped { offset: u64, bytes: u64, reason: String }, End { offset: u64, reason: String }, #[serde(other)] Unknown }`
  - `#[derive(Debug, PartialEq)] enum Action { Turn(String), Interrupt, Detach }`
  - `fn parse_action(line: &str) -> Action`
  - `fn render_frame(frame: &StreamFrame, filter: AttachFilter) -> Vec<String>`

- [ ] **Step 1: Write the failing decoder + action tests**

```rust
#[test]
fn decodes_an_event_frame_and_exposes_its_offset_and_inner_event() {
    let line = r#"{"frame":"event","session":"t/dev/1","offset":42,"event":{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"hi"}]}}}"#;
    let f: StreamFrame = serde_json::from_str(line).unwrap();
    match f {
        StreamFrame::Event { offset, event } => {
            assert_eq!(offset, 42);
            assert_eq!(event["type"], "assistant");
        }
        other => panic!("expected Event, got {other:?}"),
    }
}

#[test]
fn decodes_skipped_and_end_frames() {
    let sk: StreamFrame = serde_json::from_str(
        r#"{"frame":"skipped","session":"s","offset":9,"bytes":700000,"reason":"over_cap"}"#).unwrap();
    assert!(matches!(sk, StreamFrame::Skipped { offset: 9, bytes: 700000, .. }));
    let en: StreamFrame = serde_json::from_str(
        r#"{"frame":"end","session":"s","offset":100,"reason":"stopped"}"#).unwrap();
    assert!(matches!(en, StreamFrame::End { offset: 100, .. }));
}

#[test]
fn an_unknown_frame_kind_decodes_to_unknown_never_errors() {
    let f: StreamFrame = serde_json::from_str(r#"{"frame":"from_the_future","x":1}"#).unwrap();
    assert!(matches!(f, StreamFrame::Unknown));
}

#[test]
fn render_frame_composes_render_and_filter() {
    let line = r#"{"frame":"event","session":"s","offset":1,"event":{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t","name":"Bash","input":{"command":"ls"}}]}}}"#;
    let f: StreamFrame = serde_json::from_str(line).unwrap();
    assert!(render_frame(&f, AttachFilter::Edits).is_empty()); // Edits hides a Bash tool_use
    let lines = render_frame(&f, AttachFilter::Tools);          // Tools shows it
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("ls"));
}

#[test]
fn render_frame_shows_skipped_and_end_markers_regardless_of_filter() {
    let sk = StreamFrame::Skipped { offset: 9, bytes: 700000, reason: "over_cap".into() };
    let en = StreamFrame::End { offset: 100, reason: "stopped".into() };
    assert!(render_frame(&sk, AttachFilter::Edits)[0].to_lowercase().contains("skipped"));
    assert!(render_frame(&en, AttachFilter::Text)[0].to_lowercase().contains("stopped"));
}

#[test]
fn parse_action_maps_lines_to_turns_interrupts_and_detach() {
    assert_eq!(parse_action("fix the build"), Action::Turn("fix the build".into()));
    assert_eq!(parse_action("/interrupt"), Action::Interrupt);
    assert_eq!(parse_action("/q"), Action::Detach);
    assert_eq!(parse_action("  /q  "), Action::Detach);
    assert_eq!(parse_action(""), Action::Detach); // a blank line is a detach-safe no-op
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p camp --lib -- cmd::attach::tests`
Expected: FAIL — `StreamFrame`, `Action`, `parse_action`, `render_frame` undefined.

- [ ] **Step 3: Implement the decoder, `render_frame`, and `parse_action`**

Add to `crates/camp/src/cmd/attach.rs`:

```rust
/// One frame off the `session.subscribe` wire (cp-1). Lenient -- an unknown
/// `frame` is ignored, never a crash (the client renders campd's protocol; it
/// does not validate it). `offset` is the durable §9 resume cursor.
#[derive(Debug, Deserialize)]
#[serde(tag = "frame", rename_all = "snake_case")]
pub enum StreamFrame {
    Event { offset: u64, event: serde_json::Value },
    Skipped { offset: u64, bytes: u64, reason: String },
    End { offset: u64, reason: String },
    #[serde(other)]
    Unknown,
}

/// A steering action parsed from an operator input line (§6: turns and
/// decisions, not keypresses). The permission-answer actions (`/allow`,
/// `/deny`) drop in here when cp-3's `session.permission_decision` verb lands.
#[derive(Debug, PartialEq)]
pub enum Action {
    Turn(String),
    Interrupt,
    Detach,
}

/// Map an input line to an action. A blank line or `/q` detaches; `/interrupt`
/// interrupts; anything else is a turn.
pub fn parse_action(line: &str) -> Action {
    match line.trim() {
        "" | "/q" | "/quit" => Action::Detach,
        "/interrupt" => Action::Interrupt,
        other => Action::Turn(other.to_owned()),
    }
}

/// Turn one frame into the ready-to-print lines under `filter`. `Event` frames
/// go through `render_event` + the filter; `Skipped`/`End` render their own
/// marker lines UNCONDITIONALLY (they are stream structure, not agent content).
pub fn render_frame(frame: &StreamFrame, filter: AttachFilter) -> Vec<String> {
    match frame {
        StreamFrame::Event { event, .. } => render_event(event)
            .into_iter()
            .filter(|r| filter.matches(r) && !r.line.is_empty())
            .map(|r| r.line)
            .collect(),
        StreamFrame::Skipped { bytes, reason, .. } => {
            vec![format!("  [skipped {bytes} bytes: {reason}]")]
        }
        StreamFrame::End { reason, .. } => vec![format!("-- session {reason} --")],
        StreamFrame::Unknown => vec![],
    }
}
```

- [ ] **Step 4: Run the decoder/action tests + gates**

Run: `cargo test -p camp --lib -- cmd::attach::tests`
Expected: PASS.
Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/camp/src/cmd/attach.rs
git commit -m "feat(cli): the attach frame decoder + action parser -- wire to rendered lines, lines to steering (cp-4)"
```

---

## Task 4: The `camp attach` client IO + `main.rs` wiring

The thin glue that turns the pure logic into a running command: connect, `session.subscribe` from the chosen cursor, print the hello's start offset, spawn a reader thread that renders frames as they arrive, and run a stdin action loop that sends turns / interrupts and detaches. Detach drops the stream socket; campd sees the peer gone and the worker is UNAFFECTED (cp-1: a normal detach is `FlushStep::Gone`, silent, no event).

**Files:**
- Modify: `crates/camp/src/cmd/attach.rs`
- Modify: `crates/camp/src/main.rs` (additive: `pub mod attach;`, `Attach` variant, dispatch arm)
- Test: `crates/camp/src/cmd/attach.rs` (unit test for `subscribe_cursor`)

**Interfaces:**
- Consumes: `socket::{self, Request, Response}`, `CampDir`, Tasks 1-3.
- Produces:
  - `pub fn run(camp: &CampDir, session: String, only: AttachFilter, tail: bool, from: Option<u64>) -> anyhow::Result<()>`
  - `fn subscribe_cursor(tail: bool, from: Option<u64>) -> Option<u64>`

- [ ] **Step 1: Write the failing cursor-policy test**

```rust
#[test]
fn subscribe_cursor_policy_from_wins_then_tail_then_replay_from_zero() {
    assert_eq!(subscribe_cursor(false, Some(64)), Some(64), "--from wins");
    assert_eq!(subscribe_cursor(true, Some(64)), Some(64), "--from still wins over --tail");
    assert_eq!(subscribe_cursor(true, None), None, "--tail = live only");
    assert_eq!(subscribe_cursor(false, None), Some(0), "default = history then follow (replay)");
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p camp --lib -- cmd::attach::tests::subscribe_cursor`
Expected: FAIL — `subscribe_cursor` undefined.

- [ ] **Step 3: Implement `subscribe_cursor` and `run`**

Add to `crates/camp/src/cmd/attach.rs`:

```rust
/// The start-cursor policy: `--from <offset>` (a durable §9 resume cursor) wins;
/// else `--tail` means live-only (`None` -> campd starts at the tail); else the
/// default is `Some(0)` -- the full history, then follow (replay of a finished
/// session is exactly this on a session that ends).
fn subscribe_cursor(tail: bool, from: Option<u64>) -> Option<u64> {
    match (from, tail) {
        (Some(off), _) => Some(off),
        (None, true) => None,
        (None, false) => Some(0),
    }
}

pub fn run(
    camp: &CampDir,
    session: String,
    only: AttachFilter,
    tail: bool,
    from: Option<u64>,
) -> Result<()> {
    let cursor = subscribe_cursor(tail, from);

    // Connect + subscribe. The hello is bounded by REQUEST_TIMEOUT (a wedged
    // campd fails fast, like every verb); a down campd is the standard loud
    // error. A pure client never starts campd.
    let path = camp.socket_path();
    let mut stream = match UnixStream::connect(&path) {
        Ok(s) => s,
        Err(_) => {
            socket::require(camp, &Request::SessionSubscribe { session: session.clone(), cursor })?;
            return Ok(()); // unreachable -- require errored -- keeps the type total
        }
    };
    stream.set_read_timeout(Some(socket::REQUEST_TIMEOUT))?;
    stream.set_write_timeout(Some(socket::REQUEST_TIMEOUT))?;
    let mut line = serde_json::to_string(&Request::SessionSubscribe { session: session.clone(), cursor })?;
    line.push('\n');
    stream.write_all(line.as_bytes())?;

    let mut reader = BufReader::new(stream);
    let mut hello = String::new();
    reader.read_line(&mut hello)?;
    match serde_json::from_str::<Response>(hello.trim_end()) {
        Ok(Response::Subscribed { ok: true, cursor: c, .. }) => {
            eprintln!("attached to {session} from byte offset {c} (/q to detach, /interrupt to stop the turn)");
        }
        Ok(Response::Error { error, .. }) => bail!("campd refused session.subscribe: {error}"),
        other => bail!("unexpected session.subscribe hello: {other:?}"),
    }
    // Long-lived now: no read timeout (a quiet stream is not a wedged daemon -- §4.4).
    reader.get_ref().set_read_timeout(None)?;

    // The stream reader runs on its own thread so the main thread can read stdin
    // for steering. It OWNS the reader; on EOF (`end` frame flush, campd closing
    // the stream, or our own detach dropping the socket) it returns.
    let printer = std::thread::spawn(move || -> Result<()> {
        loop {
            let mut frame_line = String::new();
            let n = reader.read_line(&mut frame_line)?;
            if n == 0 {
                return Ok(()); // campd closed the stream (or we detached)
            }
            let trimmed = frame_line.trim_end();
            if trimmed.is_empty() {
                continue;
            }
            match serde_json::from_str::<StreamFrame>(trimmed) {
                Ok(frame) => {
                    for out in render_frame(&frame, only) {
                        println!("{out}");
                    }
                    if matches!(frame, StreamFrame::End { .. }) {
                        return Ok(()); // finished session: replay complete
                    }
                }
                Err(e) => bail!("malformed stream frame {trimmed:?}: {e}"),
            }
        }
    });

    // The steering loop: a line is a turn, `/interrupt` interrupts, `/q`/EOF
    // detaches. Each action is a SEPARATE one-shot connection (the subscribe
    // socket is server-push only) -- reusing the proven verbs.
    let stdin = std::io::stdin();
    let mut input = String::new();
    loop {
        input.clear();
        if printer.is_finished() {
            break; // the session ended -- stop prompting
        }
        let n = stdin.lock().read_line(&mut input)?;
        if n == 0 {
            break; // EOF on stdin = detach
        }
        match parse_action(&input) {
            Action::Detach => break,
            Action::Turn(text) => {
                match socket::request_if_up(camp, &Request::SessionSendTurn { session: session.clone(), text })? {
                    Some(Response::SendTurn { via, .. }) if via == "stdin" => eprintln!("(turn delivered to {session})"),
                    Some(Response::SendTurn { .. }) => eprintln!("(no live pipe for {session}; use `camp nudge` to resume an exited session)"),
                    Some(other) => eprintln!("(unexpected send_turn response: {other:?})"),
                    None => eprintln!("(campd went away; cannot deliver the turn)"),
                }
            }
            Action::Interrupt => {
                match socket::request_if_up(camp, &Request::SessionInterrupt { session: session.clone() })? {
                    Some(Response::Interrupt { request_id, .. }) => eprintln!("(interrupt sent to {session}, request {request_id})"),
                    Some(other) => eprintln!("(unexpected interrupt response: {other:?})"),
                    None => eprintln!("(campd went away; cannot interrupt)"),
                }
            }
        }
    }

    // Detach: only JOIN the printer when it has already finished (a session that
    // ended). For a still-live session we detach by letting the process exit --
    // the kernel closes our socket fd and campd's next flush sees the peer gone
    // (FlushStep::Gone, silent, worker unaffected). Joining a printer still
    // blocked in read_line would hang, so we must not.
    if printer.is_finished() {
        match printer.join() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(e),
            Err(_) => {} // the printer cannot panic (no unwrap/expect in it)
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Wire the module, command variant, and dispatch in `main.rs` (additive)**

In the `pub mod` block that declares `nudge`/`top`/`watch` (near `main.rs:20-34`), add `pub mod attach;` (if not already added in Task 1).

Add the command variant to `enum Command` (near `Watch`/`Nudge`; additive):

```rust
    /// Attach to one worker's live typed event stream (control-plane §5.2):
    /// tool calls, results, assistant text, usage -- rendered live. Replays the
    /// full history by default (a finished session ends); `--tail` follows live
    /// only; `--from <offset>` resumes from a durable byte cursor. While
    /// attached, a line is a turn, `/interrupt` stops the turn, `/q` detaches.
    /// campd must be running.
    Attach {
        /// The session NAME (from `camp watch` / `camp top`).
        session: String,
        /// Filter: all|text|tools|edits|failures (default all).
        #[arg(long, default_value = "all")]
        only: String,
        /// Follow live only -- skip the replayed history.
        #[arg(long)]
        tail: bool,
        /// Resume from a durable byte offset (a prior subscription's cursor).
        #[arg(long)]
        from: Option<u64>,
    },
```

Add the dispatch arm (near the `Watch`/`Nudge` arms):

```rust
        Command::Attach { session, only, tail, from } => {
            let filter = cmd::attach::AttachFilter::parse(&only)?;
            cmd::attach::run(&camp, session, filter, tail, from)
        }
```

Match the EXACT `camp` binding + `?`-propagation shape the neighbouring `Command::Nudge`/`Command::Watch` arms use (copy their preamble if any, e.g. a `let camp = ...;` guard).

- [ ] **Step 5: Build, run the client tests, gates**

Run: `cargo test -p camp --lib -- cmd::attach`
Expected: PASS (all pure-function tests, including `subscribe_cursor`).
Run: `cargo build -p camp`
Expected: clean (the `Attach` arm compiles; clap accepts the variant).
Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/camp/src/cmd/attach.rs crates/camp/src/main.rs
git commit -m "feat(cli): camp attach -- the per-agent view client over session.subscribe (cp-4)"
```

---

## Task 5: The `--include-partial-messages` spawn gate (§2.2)

Give a worker the ability to emit token deltas for a live attach WITHOUT autonomous dispatch gaining it. The three test-pinnable requirements from the ⚠ decision block above are the spine. Step A is the load-bearing core (the `build_spec` bool + the default-off pin), INDEPENDENT of the opt-in source. Step B wires the per-agent opt-in. **SHARED WITH cp-3: keep every `spawn.rs` edit inside the HeldStream arm, and the `AgentDef` change to one optional field — a rebase between cp-3 and cp-4 reconciles both flags/fields.**

**Interaction to know (not new work):** with partial messages ON, a worker "never goes quiet" — a hazard already named at `control.rs:318`, already bounded by the absolute `CONTROL_RESPONSE_CEILING` (`control.rs:338`). cp-4 adds no work there; do not be surprised by that comment.

**Files:**
- Modify: `crates/camp/src/daemon/spawn.rs` (`build_spec` signature + the HeldStream arm; the two argv pin tests)
- Modify: `crates/camp-core/src/pack.rs` (`AgentDef.partial_messages` + resolution)
- Modify: `crates/camp/src/daemon/dispatch.rs` (the single `build_spec` call site, `dispatch.rs:657`)
- Test: `crates/camp/src/daemon/spawn.rs`, `crates/camp-core/src/pack.rs`

### Step A — the `build_spec` bool + the default-off pin (source-independent)

- [ ] **Step A1: Update the two existing argv pin tests to assert default-OFF, and add the flag-ON test**

The HeldStream argv is pinned by `stream_argv_matches_probe_p2_and_the_fixture_facts` (`spawn.rs:1162`). `build_spec` will gain a trailing `include_partial_messages: bool`. Change that test's `build_spec(...)` call to pass `false` (default autonomous dispatch) and ADD (requirement 1, the §2.2 "must NOT gain it" half):

```rust
    // §2.2: autonomous dispatch must NOT gain --include-partial-messages.
    // MUTATION: an unconditional append leaks token deltas into unattended
    // dispatch -- this pin goes RED.
    assert!(
        !spec.argv.iter().any(|a| a == "--include-partial-messages"),
        "default (autonomous) dispatch must not emit token deltas: {:?}", spec.argv
    );
```

Add a NEW test (requirement 2 — flag ON, right arm, right position):

```rust
/// §2.2: an attach-enabled spawn gains --include-partial-messages, and ONLY on
/// the HeldStream (stream-json) arm -- never on the Null/json-envelope arm.
/// MUTATION: the gate wired to the wrong arm, or dropped, fails here.
#[test]
fn partial_messages_flag_is_added_only_when_opted_in_and_only_on_the_stream_arm() {
    let agent = fully_pinned_agent(); // same construction the fixture test uses
    let spec = build_spec(
        Path::new("claude"), &agent, Path::new("/camp"), "gc-1", "camp/dev/1",
        "sid", Path::new("/t.jsonl"), Path::new("/cwd"),
        StdinMode::HeldStream, true, // include_partial_messages
    );
    let argv: Vec<String> = spec.argv.iter().map(|a| a.to_string_lossy().into_owned()).collect();
    let i = argv.iter().position(|a| a == "--include-partial-messages")
        .expect("the flag must be present when opted in");
    assert_eq!(argv[i - 2], "--input-format"); // SDK order: after --input-format stream-json
    assert_eq!(argv[i - 1], "stream-json");

    let null_spec = build_spec(
        Path::new("claude"), &agent, Path::new("/camp"), "gc-1", "camp/dev/1",
        "sid", Path::new("/t.jsonl"), Path::new("/cwd"),
        StdinMode::Null, true,
    );
    assert!(!null_spec.argv.iter().any(|a| a == "--include-partial-messages"),
        "the json-envelope arm has no deltas to gate: {:?}", null_spec.argv);
}
```

If there is no `fully_pinned_agent()` helper, build the `AgentDef` inline exactly as `argv_matches_the_fixture_facts_for_a_fully_pinned_agent` (`spawn.rs:661`) does — copy its agent construction verbatim (and add the new `partial_messages` field once Step B1 exists; until then, construct the agent literal with `partial_messages: false`).

- [ ] **Step A2: Run to verify the tests fail**

Run: `cargo test -p camp --lib -- daemon::spawn`
Expected: FAIL — `build_spec` takes 9 args, not 10.

- [ ] **Step A3: Add the parameter and the HeldStream arm append**

In `build_spec` (`spawn.rs:167-178`) add the trailing parameter (keep `#[allow(clippy::too_many_arguments)]`):

```rust
    stdin_mode: StdinMode,
    include_partial_messages: bool,
) -> SpawnSpec {
```

In the HeldStream arm (`spawn.rs:187-203`), append the flag AFTER `--input-format stream-json`, gated by the bool (keep the scope TIGHT — cp-3-shared):

```rust
                arg("--input-format");
                arg("stream-json");
                // §2.2 (cp-4): token deltas for a LIVE attach. Gated OFF by
                // default -- autonomous dispatch must NOT gain it (per-token
                // deltas nobody reads; a worker under partial messages "never
                // goes quiet", control.rs:318). Only an opted-in agent gets it.
                if include_partial_messages {
                    arg("--include-partial-messages");
                }
```

- [ ] **Step A4: Fix the other `build_spec` call sites in the test module**

Every `#[cfg(test)]` `build_spec(...)` call (`spawn.rs:663`, `:752`, `:790`, `:1164`) needs the trailing arg — pass `false` at each EXCEPT the new flag-on test. `argv_matches_the_fixture_facts_for_a_fully_pinned_agent` (Null) and `stream_argv_matches...` both pass `false`.

- [ ] **Step A5: Run the spawn tests + gates**

Run: `cargo test -p camp --lib -- daemon::spawn`
Expected: PASS — default-off pins hold, flag-on/position test passes.
Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: clean (note: `build_spec`'s prod call site in `dispatch.rs` will not compile yet — expected; Step B3 fixes it. If clippy runs the whole workspace, do Step A6's commit after Step B3, or temporarily pass `false` at `dispatch.rs:657` here and let B3 replace it with `agent.partial_messages`).

- [ ] **Step A6: Commit**

```bash
git add crates/camp/src/daemon/spawn.rs
git commit -m "feat(spawn): --include-partial-messages gate on the HeldStream arm, default off (cp-4 §2.2)"
```

### Step B — the per-agent opt-in (guarded surface, coordinated with cp-3)

- [ ] **Step B1: Add `partial_messages` to `AgentDef` + the raw parse**

In `crates/camp-core/src/pack.rs`, add to `AgentDef` (pack.rs:30-42):

```rust
    /// §2.2 (cp-4): spawn this agent's workers with --include-partial-messages so
    /// a live `camp attach` sees token deltas. Default false -- autonomous-only
    /// agents never emit deltas. Parsed from `agent.toml`'s `partial_messages`.
    pub partial_messages: bool,
```

Parse it in `parse_agent_dir` (pack.rs:95, alongside `scope`/`stall_after`/`isolation`) from the `agent.toml` `partial_messages` key (a `bool`, default false), and set it in `resolve_agent_def` (pack.rs:197-224). Follow the EXACT pattern `isolation`/`stall_after` use. Update every `AgentDef` literal in pack.rs tests (the compiler enumerates the sites). **cp-3 note:** this and cp-3's new per-agent permission field are both new optional guarded-surface fields; at rebase, both land in the same `AgentDef` and `parse_agent_dir` without disturbing each other.

- [ ] **Step B2: Write the failing resolution test (requirement 3's foundation: default-off is the agent default)**

In `pack.rs`'s test module, mirror the existing `isolation`/`stall_after` resolution tests:

```rust
#[test]
fn partial_messages_defaults_false_and_reads_from_agent_toml() {
    // Build an agent dir with `partial_messages = true` in agent.toml exactly as
    // the isolation test builds one, resolve it, assert the field is true. Then a
    // dir WITHOUT the key resolves to false. (Copy the isolation test's dir
    // scaffold verbatim -- do not invent a new one.)
}
```

Run: `cargo test -p camp-core -- partial_messages`
Expected: FAIL before B1's field/parse, PASS after.

- [ ] **Step B3: Pass the resolved opt-in at the dispatch call site**

At `crates/camp/src/daemon/dispatch.rs:657` (the single production `build_spec` call, `StdinMode::HeldStream`), pass `agent.partial_messages` as the new trailing arg. This is what makes requirement 1 real end-to-end: the default agent (no `agent.toml` key) yields `false`, so autonomous dispatch stays flag-free.

- [ ] **Step B4: Build the workspace + run the affected suites**

Run: `cargo build -p camp && cargo test -p camp-core -- partial_messages && cargo test -p camp --lib -- daemon::spawn`
Expected: clean + PASS.
Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: clean.

- [ ] **Step B5: Commit**

```bash
git add crates/camp-core/src/pack.rs crates/camp/src/daemon/dispatch.rs
git commit -m "feat(agent): per-agent partial_messages opt-in, wired to the spawn gate (cp-4 §2.2)"
```

---

## Task 6: Exit-criteria end-to-end — attach+detach unnoticed, and replay of a finished session

The two exit criteria, proven over the REAL socket against a live fake worker, reusing `tests/control.rs`'s harness. These speak the wire directly (like cp-2's e2e), not the `camp attach` binary — the client's own logic is unit-tested in Tasks 1-4; here we prove the COMPOSED behaviour. **Neither test sets `--include-partial-messages` — the exit criteria do not depend on the flag (requirement 3).**

**Harness facts (verified against `tests/control.rs`):**
- `let dir = tempfile::tempdir().unwrap(); let (root, _rig) = scaffold(dir.path(), 4);` — `dir` MUST stay in scope for the whole test.
- `let (_bead, session) = dispatch_one(&root);` — returns `(bead, session)`; bind in that order.
- `Daemon::spawn(&root, &[(env, val)])`. `FAKE_AGENT_CONTROL_LOOP=1` = a worker that keeps its stream stdin open and answers control requests (stays live). `FAKE_AGENT_EXIT_AFTER_CONTROL=1` = answers one control request then exits (→ SIGCHLD → reap).
- `connect(&root) -> UnixStream`; `request(&mut stream, r#"{...}"#) -> serde_json::Value`.
- The session.subscribe idiom already in this file (`control.rs:661+`): write `{"op":"session.subscribe","session":"…","cursor":N}`, read the `Subscribed` hello, then read `event`/`skipped`/`end` frames under a read timeout. Reuse the existing `SubConn`/`read_frame_or_eof` helper (`control.rs:760`) if present; otherwise mirror cp-2's `fleet_subscribe` helper shape.

**Files:**
- Modify: `crates/camp/tests/control.rs`
- Test: same file

- [ ] **Step 1: Write the attach+detach-unnoticed e2e test**

```rust
// ===== cp-4: camp attach (per-agent view) =================================

/// EXIT CRITERION 1: attach + detach without the worker noticing. Subscribe to a
/// LIVE worker, read at least one frame, then DETACH (drop the connection). The
/// worker must be unaffected: still `live` in the registry afterward, and a FRESH
/// subscribe still works. campd appends no fault on the detach (silent -- cp-1).
#[test]
fn attach_then_detach_leaves_the_worker_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4);
    let campd = Daemon::spawn(&root, &[("FAKE_AGENT_CONTROL_LOOP", "1")]);
    let (_bead, session) = dispatch_one(&root);

    {
        let mut sub = connect(&root);
        sub.write_all(format!(r#"{{"op":"session.subscribe","session":"{session}","cursor":0}}"#).as_bytes()).unwrap();
        sub.write_all(b"\n").unwrap();
        let mut reader = BufReader::new(sub.try_clone().unwrap());
        let mut hello = String::new();
        reader.read_line(&mut hello).unwrap();
        let v: serde_json::Value = serde_json::from_str(hello.trim_end()).unwrap();
        assert_eq!(v["ok"], true, "subscribe hello: {v}");
        assert!(v["cursor"].as_u64().is_some(), "hello carries a byte cursor (§9): {v}");
        reader.get_ref().set_read_timeout(Some(Duration::from_millis(500))).unwrap();
        let mut line = String::new();
        let _ = reader.read_line(&mut line); // read a frame if any; content is not the point
        // DETACH: drop `sub`/`reader` at end of scope.
    }

    // The worker is still live: sessions.list shows it working (the detach did not
    // disturb its held pipe or its stream file).
    let mut ctl = connect(&root);
    let resp = request(&mut ctl, r#"{"op":"sessions.list"}"#);
    let live = resp["sessions"].as_array().unwrap();
    assert!(
        live.iter().any(|s| s["name"] == session.as_str() && s["state"] == "working"),
        "the worker must still be live and working after a detach: {resp}"
    );

    // A FRESH subscribe still succeeds (the stream file was never disturbed).
    let mut sub2 = connect(&root);
    sub2.write_all(format!(r#"{{"op":"session.subscribe","session":"{session}","cursor":0}}"#).as_bytes()).unwrap();
    sub2.write_all(b"\n").unwrap();
    let mut r2 = BufReader::new(sub2);
    let mut hello2 = String::new();
    r2.read_line(&mut hello2).unwrap();
    let v2: serde_json::Value = serde_json::from_str(hello2.trim_end()).unwrap();
    assert_eq!(v2["ok"], true, "a fresh attach after a detach still works: {v2}");

    drop(campd);
}
```

MUTATION pinned: a detach that closes the worker's stdin, kills it, or corrupts the stream file → the post-detach `sessions.list` would not show it `working`, or the fresh subscribe would error. If the exact `FAKE_AGENT_CONTROL_LOOP` liveness shape differs, model the assertion on the cp-1 test that proves a worker stays live across a subscribe (search `tests/control.rs` for a `CONTROL_LOOP` + `sessions.list`/liveness pattern) — do NOT weaken to "no panic".

- [ ] **Step 2: Run it**

Run: `cargo test -p camp --test control attach_then_detach_leaves_the_worker_untouched -- --nocapture`
Expected: PASS. If it fails, debug with systematic-debugging — do not weaken the assertion.

- [ ] **Step 3: Write the replay-of-a-finished-session e2e test**

```rust
/// EXIT CRITERION 2: replay of a finished session. Attach from cursor 0 to a
/// worker that produces its stream and then EXITS; read the full byte history
/// back (the durable replay) and the terminal `end` frame. Replay over the
/// RETAINED stream file (§9); cp-1's closing/Disposed keeps the fd alive through
/// disposal so an attached-from-0 subscriber finishes the history.
#[test]
fn replay_from_zero_delivers_the_full_history_and_an_end_frame() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 4);
    let campd = Daemon::spawn(&root, &[("FAKE_AGENT_EXIT_AFTER_CONTROL", "1")]);
    let (_bead, session) = dispatch_one(&root);

    // Attach from the START while the session is still tailed.
    let mut sub = connect(&root);
    sub.write_all(format!(r#"{{"op":"session.subscribe","session":"{session}","cursor":0}}"#).as_bytes()).unwrap();
    sub.write_all(b"\n").unwrap();
    let mut reader = BufReader::new(sub.try_clone().unwrap());
    let mut hello = String::new();
    reader.read_line(&mut hello).unwrap();
    assert_eq!(serde_json::from_str::<serde_json::Value>(hello.trim_end()).unwrap()["ok"], true);
    reader.get_ref().set_read_timeout(Some(Duration::from_millis(500))).unwrap();

    // Drive the worker to exit: interrupt it (the canonical "answer and die" path,
    // exactly as control.rs:345's cp-1 test does). Exit -> SIGCHLD -> reap -> `end`.
    {
        let mut ctl = connect(&root);
        let _ = request(&mut ctl, &format!(r#"{{"op":"session.interrupt","session":"{session}"}}"#));
    }

    // Read frames until the `end` -- with NO poke of the subscribe connection.
    let mut saw_event = false;
    let mut end_reason: Option<String> = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while std::time::Instant::now() < deadline && end_reason.is_none() {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                let t = line.trim_end();
                if t.is_empty() { continue; }
                let v: serde_json::Value = serde_json::from_str(t).unwrap();
                match v["frame"].as_str() {
                    Some("event") => {
                        assert_eq!(v["session"], session.as_str());
                        assert!(v["offset"].as_u64().is_some(), "an event frame carries a resume offset (§9): {v}");
                        saw_event = true;
                    }
                    Some("end") => end_reason = v["reason"].as_str().map(|s| s.to_owned()),
                    _ => {}
                }
            }
            Err(ref e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(e) => panic!("read: {e}"),
        }
    }
    assert!(saw_event, "the finished session's history must replay as event frames");
    assert_eq!(end_reason.as_deref(), Some("stopped"),
        "a finished session terminates the subscription with end{{reason:stopped}}");
    drop(campd);
}
```

MUTATION pinned: no `end` frame (a subscriber never learns the session finished), or a replay that skips the history (subscribe-from-tail instead of from-0). If `FAKE_AGENT_EXIT_AFTER_CONTROL` yields `crashed` rather than `stopped` in your tree, assert the `end` frame's presence and that `reason` is one of `{"stopped","crashed"}` — the terminal frame is the criterion; the exact reason is the fake's exit path.

- [ ] **Step 4: Run it, then the whole control file + gates**

Run: `cargo test -p camp --test control replay_from_zero_delivers_the_full_history_and_an_end_frame -- --nocapture`
Expected: PASS.
Run: `cargo test -p camp --test control`
Expected: PASS — all cp-0/cp-1/cp-2 e2e tests still green.
Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/camp/tests/control.rs
git commit -m "test(cp-4): camp attach end to end -- detach leaves the worker untouched; replay of a finished session (cp-4)"
```

---

## Task 7: Final verification + manual smoke

- [ ] **Step 1: Full workspace gates**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```
Expected: all green.

- [ ] **Step 2: Manual smoke of the exit criteria (the real binary)**

Bring up a camp with a dispatched worker (`camp daemon` in one shell), then in another: `camp attach <session>`. Confirm:
- The live typed stream renders (tool calls, results, assistant text, usage).
- `camp attach <session> --only edits` shows only edit-family tool calls.
- Typing a line delivers a turn (`(turn delivered …)`); `/interrupt` sends an interrupt; `/q` detaches and the worker keeps running (`camp watch` still lists it).
- The client opens NO session files: `lsof -p "$(pgrep -f 'camp attach')"` shows exactly one unix socket, no session `.json`/`.jsonl` — proving §4.2.
- For replay: attach to a session, let it finish; the history replays and the view ends with `-- session stopped --`.

- [ ] **Step 3: Perf note (no new arm required)**

cp-4 adds NO standing daemon cost: an attached session is a cp-1 `session.subscribe` subscriber, already covered by cp-1's idle perf gate (`idle_campd_with_tailed_workers_zero_cpu_under_20mb`, which holds N `session.subscribe` connections idle). cp-4 adds no new subscriber KIND and no new wake source, so the §4.3 bound is unchanged. State this in the PR description rather than adding a redundant perf arm.

---

## Self-review checklist (run before hand-off)

1. **Spec coverage.**
   - §5.2 live typed event stream (tool calls + inputs, tool results, assistant text, token usage) → Task 1 renderer (each kind tested). ✅
   - §5.2 filter → Task 2 `AttachFilter` + `--only`. ✅
   - §5.2 replay ("scrub back through a finished session") → Tasks 4 (`subscribe_cursor` default `Some(0)`) + 6 (replay e2e). Source = retained stream file per §9 (⚠ decision, pending operator); transcript replay deferred. ✅
   - §5.2 send-turn + interrupt → Task 3 `parse_action` + Task 4 action loop over `session.send_turn`/`session.interrupt`. ✅
   - §5.2 "answer a permission request" → DEFERRED to cp-3; rendered-but-not-wired (Task 1 `Permission` kind; scoping decision 3). ✅
   - §5.2 "detach freely; the worker neither knows nor cares" → Task 4 detach + Task 6 exit-criterion 1. ✅
   - §2.2 `--include-partial-messages` gates deltas; autonomous dispatch must NOT gain them → Task 5 (three named-mutation pins). ✅
   - §9 cursors are byte offsets; reaped stream is an explicit error → Tasks 3/4 (offset-carrying frames, `--from` resume) + campd's existing reaped-session error surfaced by the client. ✅
   - Exit criteria (attach+detach unnoticed; replay of a finished session; CI green) → Task 6 + Task 7. ✅
2. **Placeholder scan.** No `TBD`/`TODO`/"add error handling"/"similar to Task N". The only prose-directed steps are Task 5 B1/B2 ("mirror the isolation resolution test/pattern") and Task 6's "model on the cp-1 CONTROL_LOOP liveness test if the fake's shape differs" — each names the exact existing site to copy. Every code step carries complete code.
3. **Type consistency.** `EventKind`/`Rendered`/`render_event`/`tool_summary` (T1) → `AttachFilter`/`EDIT_TOOLS` (T2) → `StreamFrame`/`Action`/`parse_action`/`render_frame` (T3) → `subscribe_cursor`/`run` (T4) line up. `render_frame(&StreamFrame, AttachFilter) -> Vec<String>` and `AttachFilter::matches(&Rendered) -> bool` are used identically downstream. `build_spec`'s new trailing `include_partial_messages: bool` (T5) is threaded to every call site (one prod, five test). `AgentDef.partial_messages` (T5 B1) matches the dispatch call site (B3).

## Notes for the implementer

- **The daemon side is DONE by cp-1.** Resist adding any `session.subscribe`/replay machinery in `control.rs`/`event_loop.rs`/`read_channel.rs` — cp-4 consumes them verbatim. The ONLY non-client change is Task 5's spawn gate.
- **Task 5 is the cp-3-shared risk.** Keep every `spawn.rs` edit inside the `StdinMode::HeldStream` arm and the two pin tests, and the `AgentDef` change to one optional field. After cp-3 merges, the lead calls a rebase: reconcile so both `--include-partial-messages` (cp-4, gated) and `--permission-prompt-tool stdio` (cp-3) sit in the HeldStream arm, and both new optional fields sit in `AgentDef`, without disturbing each other's pins.
- **Detach must stay silent.** Do not append any event when the operator detaches — cp-1 makes a peer-gone flush silent (`control.rs:3833-3835`), and Task 6's criterion-1 test would fail a spurious event.
- **The permission line must survive every filter** (Task 2). A `can_use_tool` is a question addressed to the operator; no `--only` value may hide it. This is the seam cp-3's answer action drops into.
- **`subscribe_cursor` default is `Some(0)`, not `None`.** History-then-follow is the useful default and IS the replay path for a finished session — one code path, two behaviours. `--tail` is the opt-out.

## Known gaps (state, do not close in cp-4)

- **Post-reap transcript replay is deferred** (⚠ replay decision, pending operator). A reaped/disposed session's subscribe is the explicit §9 error; genuine `.jsonl`-transcript replay is a follow-up (new verb + claude-transcript format adapter behind the same renderer).
- **Partial-message delta RENDERING is lenient, not rich.** With the flag on, `stream_event`/`content_block_delta` events render through the `Other` path rather than as smooth incremental text. Rich delta rendering is additive on top of Task 1; the exit criteria and §5.2's listed content need only complete events.
- **The steering loop is not unit-tested end-to-end** (threads + stdin). Its pure pieces (`parse_action`, `render_frame`, `subscribe_cursor`) are tested; the sends reuse verbs proven by cp-1's e2e. A manual smoke (Task 7 Step 2) covers the composed loop.
- **`--from <offset>` is not exercised by an automated cp-4 test.** The cursor policy is unit-tested; resuming a real subscription from a mid-stream durable offset is covered transitively by cp-1's resume-from-offset subscriber tests. A dedicated cp-4 e2e is a candidate follow-up.

//! `camp attach <session>` (control-plane spec §5.2): the per-agent view. A
//! STATELESS RENDERER (§4.2): it opens a `session.subscribe` stream and renders
//! one worker's typed events live — tool calls with inputs, tool results,
//! assistant text, token usage. It never opens a session file, never learns a
//! pid; it reaches the worker only through the socket. Replay and live-follow are
//! the SAME subscribe (cursor 0 replays history then follows; a finished session
//! ends). From here you do §5.2's three things: send a turn, interrupt, and
//! ANSWER A PERMISSION — `/allow`, `/allow_always`, `/deny <reason>` on the line
//! loop (issue #120), validated by `camp decide`'s rules and delivered over the
//! same `session.permission_decision` verb. The one-shot `camp decide` remains
//! the out-of-band answer for an operator who is not attached.

use std::io::{BufRead, BufReader, Write};
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow, bail};
use serde::Deserialize;

use crate::campdir::CampDir;
use crate::cmd::decide;
use crate::daemon::socket::{self, Request, Response};

/// The KIND of a rendered line — what the `--only` filter selects on.
#[derive(Debug, Clone, PartialEq)]
pub enum EventKind {
    Text,
    ToolUse {
        tool: String,
    },
    ToolResult {
        is_error: bool,
    },
    Result,
    /// A `can_use_tool` control request — a question addressed to the operator.
    ///
    /// It CARRIES the `request_id` the answer must quote, so the view's rendered
    /// BLOCKED line and the id an in-view `/allow` sends come from ONE parse of
    /// ONE frame — the operator can never answer an id different from the one
    /// they are looking at. `None` is a control_request with no id on the wire:
    /// still rendered (nothing hidden), but unanswerable, so no `/allow` can
    /// quote it.
    Permission {
        request_id: Option<String>,
    },
    System,
    /// STREAM STRUCTURE, not agent content: the `skipped`/`end` markers. Like a
    /// permission, no filter may hide one — a `--only text` view that silently
    /// dropped "the session ended" would be lying about the stream.
    Stream,
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
                                out.push(Rendered {
                                    kind: EventKind::Text,
                                    line: t.to_owned(),
                                });
                            }
                        }
                    }
                    "tool_use" => {
                        let name = b.get("name").and_then(|v| v.as_str()).unwrap_or("tool");
                        let empty = serde_json::Value::Null;
                        let input = b.get("input").unwrap_or(&empty);
                        out.push(Rendered {
                            kind: EventKind::ToolUse {
                                tool: name.to_owned(),
                            },
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
                    _ => out.push(Rendered {
                        kind: EventKind::Other,
                        line: String::new(),
                    }),
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
            let cost = ev
                .get("total_cost_usd")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
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
                // §5.4/§5.3: the request_id rides THIS frame (top-level) — the
                // only socket surface that carries it, since SessionInfo has no
                // such field. Render it (an out-of-band `camp decide` needs it),
                // and name the answer the operator can type RIGHT HERE (#120).
                let request_id = ev
                    .get("request_id")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned);
                let shown = request_id.clone().unwrap_or_else(|| "?".to_owned());
                vec![Rendered {
                    kind: EventKind::Permission { request_id },
                    line: format!(
                        "  !! BLOCKED -- {tool} needs your decision -- request {shown} \
                         -- answer: /allow, /allow_always, or /deny <reason>"
                    ),
                }]
            } else {
                vec![Rendered {
                    kind: EventKind::Other,
                    line: String::new(),
                }]
            }
        }
        "system" => vec![Rendered {
            kind: EventKind::System,
            line: String::new(),
        }],
        _ => vec![Rendered {
            kind: EventKind::Other,
            line: String::new(),
        }],
    }
}

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
            other => {
                bail!("unknown --only filter {other:?}: expected all|text|tools|edits|failures")
            }
        }
    }

    /// Does this line pass? A `Permission` or `Stream` line ALWAYS passes -- a
    /// question addressed to the operator, and the shape of the stream itself,
    /// must never be filtered away.
    pub fn matches(&self, r: &Rendered) -> bool {
        if matches!(r.kind, EventKind::Permission { .. } | EventKind::Stream) {
            return true;
        }
        match self {
            AttachFilter::All => true,
            AttachFilter::Text => r.kind == EventKind::Text,
            AttachFilter::Tools => {
                matches!(
                    r.kind,
                    EventKind::ToolUse { .. } | EventKind::ToolResult { .. }
                )
            }
            AttachFilter::Edits => {
                matches!(&r.kind, EventKind::ToolUse { tool } if EDIT_TOOLS.contains(&tool.as_str()))
            }
            AttachFilter::Failures => {
                matches!(r.kind, EventKind::ToolResult { is_error: true })
                    || r.kind == EventKind::Result && r.line.to_lowercase().contains("error")
            }
        }
    }
}

/// One frame off the `session.subscribe` wire (cp-1). Lenient -- an unknown
/// `frame` is ignored, never a crash (the client renders campd's protocol; it
/// does not validate it). `offset` is the durable §9 resume cursor.
#[derive(Debug, Deserialize)]
#[serde(tag = "frame", rename_all = "snake_case")]
pub enum StreamFrame {
    Event {
        offset: u64,
        event: serde_json::Value,
    },
    Skipped {
        offset: u64,
        bytes: u64,
        reason: String,
    },
    End {
        offset: u64,
        reason: String,
    },
    #[serde(other)]
    Unknown,
}

impl StreamFrame {
    /// The durable §9 resume cursor this frame carries -- the byte offset of the
    /// START OF THE NEXT LINE. `None` for an `Unknown` frame (no offset to trust).
    /// Surfaced to the operator so a later `camp attach --from <offset>` can pick
    /// up exactly where a detach left off.
    pub fn offset(&self) -> Option<u64> {
        match self {
            StreamFrame::Event { offset, .. }
            | StreamFrame::Skipped { offset, .. }
            | StreamFrame::End { offset, .. } => Some(*offset),
            StreamFrame::Unknown => None,
        }
    }
}

/// A steering action parsed from an operator input line (§6: turns and
/// decisions, not keypresses — a `/deny <reason>` LINE is a decision, which is
/// exactly what §6 says this loop carries).
#[derive(Debug, PartialEq)]
pub enum Action {
    Turn(String),
    Interrupt,
    Detach,
    /// A permission answer typed at a BLOCKED worker (§5.2/#120): `/allow`,
    /// `/allow_always`, or `/deny <reason>`.
    ///
    /// The request_id is NOT here BY DESIGN — the operator never retypes an id
    /// the view already read off the frame it rendered for them. `reason` is
    /// everything after the verb; whether it is REQUIRED is `camp decide`'s rule
    /// to enforce (`decide::decision_request`), never a second copy here.
    Decide {
        decision: String,
        reason: Option<String>,
    },
}

/// Map an input line to an action. A blank line or `/q` detaches; `/interrupt`
/// interrupts; a leading `/allow`//`/allow_always`//`/deny` is a permission
/// answer, carrying any trailing text as its reason; anything else is a turn.
pub fn parse_action(line: &str) -> Action {
    let trimmed = line.trim();
    let (verb, rest) = trimmed
        .split_once(char::is_whitespace)
        .unwrap_or((trimmed, ""));
    match trimmed {
        "" | "/q" | "/quit" => Action::Detach,
        "/interrupt" => Action::Interrupt,
        _ if matches!(verb, "/allow" | "/allow_always" | "/deny") => {
            let reason = rest.trim();
            Action::Decide {
                // The verb IS the decision vocabulary `camp decide` validates:
                // `/allow_always` -> "allow_always". No mapping table to drift.
                decision: verb.trim_start_matches('/').to_owned(),
                reason: (!reason.is_empty()).then(|| reason.to_owned()),
            }
        }
        other => Action::Turn(other.to_owned()),
    }
}

/// Turn one frame into the ready-to-print lines under `filter`. `Event` frames
/// go through `render_event` + the filter; `Skipped`/`End` render their own
/// marker lines UNCONDITIONALLY (they are stream structure, not agent content).
///
/// Returns `Rendered`, not bare strings: the caller must SEE what it is printing
/// — a `Permission` line hands the view the request_id an in-attach `/allow`
/// answers (#120), and re-deriving that from the printed text, or by rendering
/// the frame a second time, would be a second source of truth.
pub fn render_frame(frame: &StreamFrame, filter: AttachFilter) -> Vec<Rendered> {
    match frame {
        StreamFrame::Event { event, .. } => render_event(event)
            .into_iter()
            .filter(|r| filter.matches(r) && !r.line.is_empty())
            .collect(),
        StreamFrame::Skipped { bytes, reason, .. } => vec![Rendered {
            kind: EventKind::Stream,
            line: format!("  [skipped {bytes} bytes: {reason}]"),
        }],
        StreamFrame::End { reason, .. } => vec![Rendered {
            kind: EventKind::Stream,
            line: format!("-- session {reason} --"),
        }],
        StreamFrame::Unknown => vec![],
    }
}

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
    let mut stream = match socket::connect_stream(&path) {
        Ok(s) => s,
        Err(_) => {
            socket::require(
                camp,
                &Request::SessionSubscribe {
                    session: session.clone(),
                    cursor,
                },
            )?;
            return Ok(()); // unreachable -- require errored -- keeps the type total
        }
    };
    stream.set_read_timeout(Some(socket::REQUEST_TIMEOUT))?;
    stream.set_write_timeout(Some(socket::REQUEST_TIMEOUT))?;
    let mut line = serde_json::to_string(&Request::SessionSubscribe {
        session: session.clone(),
        cursor,
    })?;
    line.push('\n');
    stream.write_all(line.as_bytes())?;

    let mut reader = BufReader::new(stream);
    let mut hello = String::new();
    reader.read_line(&mut hello)?;
    match serde_json::from_str::<Response>(hello.trim_end()) {
        Ok(Response::Subscribed {
            ok: true,
            cursor: c,
            ..
        }) => {
            eprintln!(
                "attached to {session} from byte offset {c} (/q to detach, /interrupt to stop the turn)"
            );
        }
        Ok(Response::Error { error, .. }) => bail!("campd refused session.subscribe: {error}"),
        other => bail!("unexpected session.subscribe hello: {other:?}"),
    }
    // Long-lived now: no read timeout (a quiet stream is not a wedged daemon -- §4.4).
    reader.get_ref().set_read_timeout(None)?;

    // The BLOCKED question currently on the floor, as the PRINTER read it off the
    // wire and the STEERING loop answers it (#120). This slot is why `/allow`
    // needs no typed request_id: the view already saw the id it is showing.
    let pending: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let pending_writer = Arc::clone(&pending);

    // The stream reader runs on its own thread so the main thread can read stdin
    // for steering. It OWNS the reader; on EOF (`end` frame flush, campd closing
    // the stream, or our own detach dropping the socket) it returns.
    let printer = std::thread::spawn(move || -> Result<()> {
        // The last durable §9 offset seen -- reported on `end` so the operator has
        // the exact `--from` cursor to resume from.
        let mut last_offset: Option<u64> = None;
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
                    if let Some(off) = frame.offset() {
                        last_offset = Some(off);
                    }
                    for out in render_frame(&frame, only) {
                        // Record the question BEFORE printing it: the operator
                        // cannot answer a line they have not seen, so an id
                        // stored here is always ready by the time the BLOCKED
                        // line reaches them (same thread, program order).
                        if let EventKind::Permission {
                            request_id: Some(id),
                        } = &out.kind
                        {
                            let mut slot = pending_writer
                                .lock()
                                .map_err(|_| anyhow!("the pending-permission slot is poisoned"))?;
                            *slot = Some(id.clone());
                        }
                        println!("{}", out.line);
                    }
                    if matches!(frame, StreamFrame::End { .. }) {
                        if let Some(off) = last_offset {
                            eprintln!("(resume this replay with --from {off})");
                        }
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
                match socket::request_if_up(
                    camp,
                    &Request::SessionSendTurn {
                        session: session.clone(),
                        text,
                    },
                )? {
                    Some(Response::SendTurn { via, .. }) if via == "stdin" => {
                        eprintln!("(turn delivered to {session})")
                    }
                    Some(Response::SendTurn { .. }) => eprintln!(
                        "(no live pipe for {session}; use `camp nudge` to resume an exited session)"
                    ),
                    Some(other) => eprintln!("(unexpected send_turn response: {other:?})"),
                    None => eprintln!("(campd went away; cannot deliver the turn)"),
                }
            }
            Action::Interrupt => {
                match socket::request_if_up(
                    camp,
                    &Request::SessionInterrupt {
                        session: session.clone(),
                    },
                )? {
                    Some(Response::Interrupt { request_id, .. }) => {
                        eprintln!("(interrupt sent to {session}, request {request_id})")
                    }
                    Some(other) => eprintln!("(unexpected interrupt response: {other:?})"),
                    None => eprintln!("(campd went away; cannot interrupt)"),
                }
            }
            Action::Decide { decision, reason } => {
                // #120: answer the question this view is showing, over the same
                // `session.permission_decision` verb `camp decide` uses.
                let request_id = pending
                    .lock()
                    .map_err(|_| anyhow!("the pending-permission slot is poisoned"))?
                    .clone();
                let Some(request_id) = request_id else {
                    eprintln!(
                        "(nothing is BLOCKED on {session} right now — there is no permission to answer)"
                    );
                    continue;
                };
                match decide::decision_request(
                    session.clone(),
                    request_id.clone(),
                    decision,
                    reason,
                ) {
                    // A bare `/deny` is the OPERATOR's slip, not the session's:
                    // name the rule and stay attached. Detaching a live view over
                    // a mistyped line would cost them the stream they are
                    // answering from — and the worker is still safely BLOCKED.
                    Err(e) => eprintln!("({e:#})"),
                    Ok(request) => match socket::request_if_up(camp, &request) {
                        Ok(Some(Response::PermissionDecided {
                            decision: recorded, ..
                        })) => {
                            // Answered: the slot must not offer a stale id to the
                            // next bare `/allow`. Clear it ONLY if the worker has
                            // not already asked something new in the meantime.
                            let mut slot = pending
                                .lock()
                                .map_err(|_| anyhow!("the pending-permission slot is poisoned"))?;
                            if slot.as_deref() == Some(request_id.as_str()) {
                                *slot = None;
                            }
                            eprintln!(
                                "(recorded {recorded} for {request_id} on {session}, and delivered it to the worker)"
                            );
                        }
                        Ok(Some(other)) => {
                            eprintln!("(unexpected permission decision response: {other:?})")
                        }
                        Ok(None) => eprintln!("(campd went away; cannot deliver the decision)"),
                        // campd REFUSED — "already decided by X" is §4's
                        // first-answer-wins talking, i.e. campd ANSWERING the
                        // operator, not a fault to tear the view down over. Loud
                        // and complete (`{e:#}` carries the whole error chain),
                        // never swallowed; the operator decides what to do next.
                        Err(e) => eprintln!("(the decision was refused: {e:#})"),
                    },
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn subscribe_cursor_policy_from_wins_then_tail_then_replay_from_zero() {
        assert_eq!(subscribe_cursor(false, Some(64)), Some(64), "--from wins");
        assert_eq!(
            subscribe_cursor(true, Some(64)),
            Some(64),
            "--from still wins over --tail"
        );
        assert_eq!(subscribe_cursor(true, None), None, "--tail = live only");
        assert_eq!(
            subscribe_cursor(false, None),
            Some(0),
            "default = history then follow (replay)"
        );
    }

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
            r#"{"frame":"skipped","session":"s","offset":9,"bytes":700000,"reason":"over_cap"}"#,
        )
        .unwrap();
        assert!(matches!(
            sk,
            StreamFrame::Skipped {
                offset: 9,
                bytes: 700000,
                ..
            }
        ));
        let en: StreamFrame = serde_json::from_str(
            r#"{"frame":"end","session":"s","offset":100,"reason":"stopped"}"#,
        )
        .unwrap();
        assert!(matches!(en, StreamFrame::End { offset: 100, .. }));
    }

    #[test]
    fn an_unknown_frame_kind_decodes_to_unknown_never_errors() {
        let f: StreamFrame = serde_json::from_str(r#"{"frame":"from_the_future","x":1}"#).unwrap();
        assert!(matches!(f, StreamFrame::Unknown));
    }

    #[test]
    fn offset_exposes_the_durable_resume_cursor_for_every_real_frame() {
        let ev = StreamFrame::Event {
            offset: 42,
            event: json!({}),
        };
        let sk = StreamFrame::Skipped {
            offset: 9,
            bytes: 700000,
            reason: "over_cap".into(),
        };
        let en = StreamFrame::End {
            offset: 100,
            reason: "stopped".into(),
        };
        assert_eq!(ev.offset(), Some(42));
        assert_eq!(sk.offset(), Some(9));
        assert_eq!(en.offset(), Some(100));
        assert_eq!(StreamFrame::Unknown.offset(), None); // no offset to trust
    }

    /// The rendered LINES of a frame — the old `render_frame` shape, kept as a
    /// test helper so these assertions stay about rendering, not about `Rendered`.
    fn frame_lines(frame: &StreamFrame, filter: AttachFilter) -> Vec<String> {
        render_frame(frame, filter)
            .into_iter()
            .map(|r| r.line)
            .collect()
    }

    #[test]
    fn render_frame_composes_render_and_filter() {
        let line = r#"{"frame":"event","session":"s","offset":1,"event":{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t","name":"Bash","input":{"command":"ls"}}]}}}"#;
        let f: StreamFrame = serde_json::from_str(line).unwrap();
        assert!(render_frame(&f, AttachFilter::Edits).is_empty()); // Edits hides a Bash tool_use
        let lines = frame_lines(&f, AttachFilter::Tools); // Tools shows it
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("ls"));
    }

    #[test]
    fn render_frame_shows_skipped_and_end_markers_regardless_of_filter() {
        let sk = StreamFrame::Skipped {
            offset: 9,
            bytes: 700000,
            reason: "over_cap".into(),
        };
        let en = StreamFrame::End {
            offset: 100,
            reason: "stopped".into(),
        };
        assert!(
            frame_lines(&sk, AttachFilter::Edits)[0]
                .to_lowercase()
                .contains("skipped")
        );
        assert!(
            frame_lines(&en, AttachFilter::Text)[0]
                .to_lowercase()
                .contains("stopped")
        );
    }

    /// #120's load-bearing wiring: `render_frame` hands the CALLER the
    /// request_id, so the printer thread can stock the slot a bare `/allow`
    /// answers. A `Permission` line that renders the id for human eyes but drops
    /// it from the `kind` would leave the steering loop with nothing to send —
    /// and the operator back in a second terminal.
    #[test]
    fn render_frame_hands_the_caller_the_blocked_request_id() {
        let wire = r#"{"frame":"event","session":"s","offset":1,"event":{"type":"control_request","request_id":"cli-gc-7","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"cargo publish"}}}}"#;
        let f: StreamFrame = serde_json::from_str(wire).unwrap();
        // Under EVERY filter — the id must survive wherever the question does.
        for filter in [
            AttachFilter::All,
            AttachFilter::Text,
            AttachFilter::Tools,
            AttachFilter::Edits,
            AttachFilter::Failures,
        ] {
            let rendered = render_frame(&f, filter);
            assert_eq!(rendered.len(), 1, "{filter:?} lost the permission line");
            assert_eq!(
                rendered[0].kind,
                EventKind::Permission {
                    request_id: Some("cli-gc-7".into())
                },
                "{filter:?} did not carry the request_id to the caller"
            );
        }
    }

    /// The id the view SHOWS and the id an in-view `/allow` SENDS are one parse
    /// of one frame: they cannot drift apart.
    #[test]
    fn the_rendered_id_and_the_carried_id_are_the_same_id() {
        let ev = json!({
            "type": "control_request", "request_id": "cli-gc-42",
            "request": {"subtype": "can_use_tool", "tool_name": "Bash", "input": {}}
        });
        let r = render_event(&ev);
        let EventKind::Permission { request_id } = &r[0].kind else {
            panic!("expected a Permission kind, got {:?}", r[0].kind);
        };
        let carried = request_id.as_deref().expect("the id must be carried");
        assert!(
            r[0].line.contains(carried),
            "the line shows an id the kind does not carry: {:?}",
            r[0]
        );
    }

    /// A control_request with NO request_id on the wire: still RENDERED (nothing
    /// hidden — the operator must see the worker is stuck), but unanswerable, so
    /// the kind carries no id for `/allow` to quote. Inventing a placeholder id
    /// here would make attach send campd a request_id that never existed.
    #[test]
    fn a_permission_with_no_wire_id_renders_but_carries_no_answerable_id() {
        let ev = json!({
            "type": "control_request",
            "request": {"subtype": "can_use_tool", "tool_name": "Bash", "input": {}}
        });
        let r = render_event(&ev);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].kind, EventKind::Permission { request_id: None });
        assert!(r[0].line.to_uppercase().contains("BLOCKED"), "{:?}", r[0]);
    }

    /// CP4's OWNED half of the "replay of a finished session" exit criterion: the
    /// CLIENT renders a WHOLE finished-session replay — every history line in order,
    /// then the terminal marker. The DAEMON's full-history-then-end delivery over a
    /// cursor-0 subscribe is cp-1's guarantee (tests/control.rs:1387, cited in Task
    /// 6); this proves the piece cp-1 cannot: that the client turns that byte stream
    /// into the operator's scrollback WITHOUT truncating it. It pins exactly the
    /// mutation the gate flagged (a replay that drops history frames): decode each
    /// wire line, render it, and assert the full ordered transcript + `-- session
    /// stopped --` appear.
    #[test]
    fn client_renders_a_full_finished_replay_in_order_then_the_end_marker() {
        // A realistic cursor-0 replay: an init/text line, a tool call, its result,
        // the worker's terminal answer, then the end frame — exactly the frame
        // sequence tests/control.rs:1387 delivers over the wire.
        let wire = [
            r#"{"frame":"event","session":"s","offset":10,"event":{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"starting"}]}}}"#,
            r#"{"frame":"event","session":"s","offset":40,"event":{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t","name":"Bash","input":{"command":"make"}}]}}}"#,
            r#"{"frame":"event","session":"s","offset":90,"event":{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t","content":"ok","is_error":false}]}}}"#,
            r#"{"frame":"event","session":"s","offset":140,"event":{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"done here"}]}}}"#,
            r#"{"frame":"end","session":"s","offset":140,"reason":"stopped"}"#,
        ];
        let mut out: Vec<String> = Vec::new();
        for line in wire {
            let f: StreamFrame = serde_json::from_str(line).unwrap();
            out.extend(frame_lines(&f, AttachFilter::All));
        }
        // The FULL history rendered, IN ORDER — not a truncated prefix.
        let joined = out.join("\n");
        let starting = out
            .iter()
            .position(|l| l.contains("starting"))
            .expect("first line");
        let make = out
            .iter()
            .position(|l| l.contains("make"))
            .expect("tool call");
        let done = out
            .iter()
            .position(|l| l.contains("done here"))
            .expect("terminal answer");
        assert!(
            starting < make && make < done,
            "history must render in order: {out:#?}"
        );
        assert!(
            joined.contains("ok"),
            "the tool result is part of the replay: {out:#?}"
        );
        // The terminal marker is LAST — the operator sees the session finished.
        assert!(
            out.last().unwrap().contains("session stopped"),
            "ends with the terminal marker: {out:#?}"
        );
    }

    #[test]
    fn parse_action_maps_lines_to_turns_interrupts_and_detach() {
        assert_eq!(
            parse_action("fix the build"),
            Action::Turn("fix the build".into())
        );
        assert_eq!(parse_action("/interrupt"), Action::Interrupt);
        assert_eq!(parse_action("/q"), Action::Detach);
        assert_eq!(parse_action("  /q  "), Action::Detach);
        assert_eq!(parse_action(""), Action::Detach); // a blank line is a detach-safe no-op
    }

    /// A permission answer typed at a BLOCKED worker parses into the DECISION
    /// VOCABULARY `camp decide` validates, with any trailing text as the reason
    /// (#120). Before this, every one of these lines collapsed to a hint that
    /// told the operator to go open another terminal.
    #[test]
    fn parse_action_maps_a_permission_keypress_to_its_decision_and_reason() {
        let decide = |decision: &str, reason: Option<&str>| Action::Decide {
            decision: decision.into(),
            reason: reason.map(str::to_owned),
        };
        assert_eq!(parse_action("/allow"), decide("allow", None));
        assert_eq!(parse_action("/allow_always"), decide("allow_always", None));
        // The whole tail is the reason — a reason is a sentence, not one word.
        assert_eq!(
            parse_action("/deny not safe on prod"),
            decide("deny", Some("not safe on prod"))
        );
        // A bare `/deny` parses; the MISSING REASON is `decide`'s rule to catch,
        // not something this parser silently invents a value for.
        assert_eq!(parse_action("/deny"), decide("deny", None));
        assert_eq!(parse_action("  /allow  "), decide("allow", None)); // surrounding space
        assert_eq!(
            parse_action("  /deny  too risky  "),
            decide("deny", Some("too risky"))
        );
        // a plain turn that merely MENTIONS allow is still a turn — only the
        // leading `/allow`//deny keypress is intercepted.
        assert_eq!(
            parse_action("allow the build to run"),
            Action::Turn("allow the build to run".into())
        );
    }

    /// The verb IS the wire vocabulary: `/allow_always` must send "allow_always",
    /// never "allow". The three decisions are NOT interchangeable — `allow_always`
    /// widens the worker's allowlist for the rest of the session, and silently
    /// downgrading it to a one-shot `allow` (or vice versa) is a permission bug,
    /// not a typo. `decide::decision_request` is what pins the vocabulary itself.
    #[test]
    fn every_decision_verb_keeps_its_own_identity_on_the_wire() {
        for verb in ["allow", "allow_always", "deny"] {
            let line = format!("/{verb} a reason");
            assert_eq!(
                parse_action(&line),
                Action::Decide {
                    decision: verb.into(),
                    reason: Some("a reason".into()),
                },
                "{line} did not parse to the {verb} decision"
            );
        }
    }

    #[test]
    fn filter_all_admits_everything() {
        let f = AttachFilter::All;
        assert!(f.matches(&Rendered {
            kind: EventKind::Text,
            line: "x".into()
        }));
        assert!(f.matches(&Rendered {
            kind: EventKind::Result,
            line: "x".into()
        }));
    }

    #[test]
    fn filter_edits_admits_only_edit_family_tool_uses() {
        let f = AttachFilter::Edits;
        assert!(f.matches(&Rendered {
            kind: EventKind::ToolUse {
                tool: "Edit".into()
            },
            line: "x".into()
        }));
        assert!(f.matches(&Rendered {
            kind: EventKind::ToolUse {
                tool: "Write".into()
            },
            line: "x".into()
        }));
        assert!(!f.matches(&Rendered {
            kind: EventKind::ToolUse {
                tool: "Bash".into()
            },
            line: "x".into()
        }));
        assert!(!f.matches(&Rendered {
            kind: EventKind::Text,
            line: "x".into()
        }));
    }

    #[test]
    fn filter_failures_admits_error_results_only() {
        let f = AttachFilter::Failures;
        assert!(f.matches(&Rendered {
            kind: EventKind::ToolResult { is_error: true },
            line: "x".into()
        }));
        assert!(!f.matches(&Rendered {
            kind: EventKind::ToolResult { is_error: false },
            line: "x".into()
        }));
        assert!(!f.matches(&Rendered {
            kind: EventKind::Text,
            line: "x".into()
        }));
    }

    #[test]
    fn filter_tools_admits_tool_uses_and_results() {
        let f = AttachFilter::Tools;
        assert!(f.matches(&Rendered {
            kind: EventKind::ToolUse {
                tool: "Bash".into()
            },
            line: "x".into()
        }));
        assert!(f.matches(&Rendered {
            kind: EventKind::ToolResult { is_error: false },
            line: "x".into()
        }));
        assert!(!f.matches(&Rendered {
            kind: EventKind::Text,
            line: "x".into()
        }));
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
        let p = Rendered {
            kind: EventKind::Permission {
                request_id: Some("r1".into()),
            },
            line: "!! BLOCKED".into(),
        };
        // Stream structure is equally unhideable: a `--only text` view that
        // dropped "the session ended" would be lying about the stream.
        let s = Rendered {
            kind: EventKind::Stream,
            line: "-- session stopped --".into(),
        };
        for f in [
            AttachFilter::All,
            AttachFilter::Text,
            AttachFilter::Tools,
            AttachFilter::Edits,
            AttachFilter::Failures,
        ] {
            assert!(f.matches(&p), "{f:?} hid a permission line");
            assert!(f.matches(&s), "{f:?} hid a stream marker");
        }
    }

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
        assert_eq!(
            r[0].kind,
            EventKind::ToolUse {
                tool: "Edit".into()
            }
        );
        assert!(
            r[0].line.contains("Edit") && r[0].line.contains("src/lib.rs"),
            "{:?}",
            r[0]
        );
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
        assert_eq!(
            r[0].kind,
            EventKind::ToolUse {
                tool: "Bash".into()
            }
        );
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
        assert_eq!(
            r[1].kind,
            EventKind::ToolUse {
                tool: "Read".into()
            }
        );
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
        assert_eq!(
            render_event(&ok)[0].kind,
            EventKind::ToolResult { is_error: false }
        );
        assert_eq!(
            render_event(&err)[0].kind,
            EventKind::ToolResult { is_error: true }
        );
        assert!(
            render_event(&err)[0].line.to_lowercase().contains("error"),
            "error is visible"
        );
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
        assert!(
            r[0].line.contains("20") || r[0].line.to_lowercase().contains("token"),
            "{:?}",
            r[0]
        );
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
        assert_eq!(
            r[0].kind,
            EventKind::Permission {
                request_id: Some("r1".into())
            }
        );
        assert!(r[0].line.to_uppercase().contains("BLOCKED"), "{:?}", r[0]);
        // cp-5 (§5.4): the operator's answer path needs the request_id, and the
        // only socket surface that carries it is this stream frame. Render it —
        // an out-of-band `camp decide` (run from a terminal that is NOT attached)
        // still has no other way to learn it.
        let line = &r[0].line;
        assert!(
            line.contains("r1"),
            "the BLOCKED line must render the request_id: {line}"
        );
        // #120: the answer path this VIEW names is the one typable IN this view.
        // It used to name `camp decide` — correct then (attach could not answer),
        // and a lie now: it sent the operator to a second terminal for something
        // the line loop under their cursor does.
        assert!(
            line.contains("/allow") && line.contains("/deny"),
            "the BLOCKED line must name the in-view answer: {line}"
        );
        assert!(
            !line.contains("camp decide"),
            "the BLOCKED line must not send an ATTACHED operator to a second terminal: {line}"
        );
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

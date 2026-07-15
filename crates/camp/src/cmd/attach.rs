//! `camp attach <session>` (control-plane spec §5.2): the per-agent view. A
//! STATELESS RENDERER (§4.2): it opens a `session.subscribe` stream and renders
//! one worker's typed events live — tool calls with inputs, tool results,
//! assistant text, token usage. It never opens a session file, never learns a
//! pid; it reaches the worker only through the socket. Replay and live-follow are
//! the SAME subscribe (cursor 0 replays history then follows; a finished session
//! ends). From here you send a turn or interrupt. Answering a permission is
//! out-of-band via the one-shot `camp decide` (cp-3, shipped), using the
//! request_id this view renders on the BLOCKED line; an INTERACTIVE in-attach
//! `/allow`//deny action is a separate cp-4 deferral.

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
    ToolUse {
        tool: String,
    },
    ToolResult {
        is_error: bool,
    },
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
                // such field. Render it, and name the real answer path: the
                // one-shot `camp decide` (cp-3 shipped). Attach's INTERACTIVE
                // /allow-//deny loop is still cp-4's deferral - not wired here.
                let request_id = ev.get("request_id").and_then(|v| v.as_str()).unwrap_or("?");
                vec![Rendered {
                    kind: EventKind::Permission,
                    line: format!(
                        "  !! BLOCKED -- {tool} needs your decision -- request {request_id} \
                         -- answer: camp decide <session> {request_id} allow|deny"
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

    /// Does this line pass? A `Permission` line ALWAYS passes -- a question
    /// addressed to the operator must never be filtered away.
    pub fn matches(&self, r: &Rendered) -> bool {
        if r.kind == EventKind::Permission {
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
/// decisions, not keypresses). The overseer answers a permission out-of-band
/// with the one-shot `camp decide` (cp-3, shipped) using the request_id this
/// view renders on the BLOCKED line; wiring an INTERACTIVE `/allow`//deny
/// action into this loop remains a separate cp-4 deferral.
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
                        println!("{out}");
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

    #[test]
    fn render_frame_composes_render_and_filter() {
        let line = r#"{"frame":"event","session":"s","offset":1,"event":{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t","name":"Bash","input":{"command":"ls"}}]}}}"#;
        let f: StreamFrame = serde_json::from_str(line).unwrap();
        assert!(render_frame(&f, AttachFilter::Edits).is_empty()); // Edits hides a Bash tool_use
        let lines = render_frame(&f, AttachFilter::Tools); // Tools shows it
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
            render_frame(&sk, AttachFilter::Edits)[0]
                .to_lowercase()
                .contains("skipped")
        );
        assert!(
            render_frame(&en, AttachFilter::Text)[0]
                .to_lowercase()
                .contains("stopped")
        );
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
            out.extend(render_frame(&f, AttachFilter::All));
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
            kind: EventKind::Permission,
            line: "!! BLOCKED".into(),
        };
        for f in [
            AttachFilter::All,
            AttachFilter::Text,
            AttachFilter::Tools,
            AttachFilter::Edits,
            AttachFilter::Failures,
        ] {
            assert!(f.matches(&p), "{f:?} hid a permission line");
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
        assert_eq!(r[0].kind, EventKind::Permission);
        assert!(r[0].line.to_uppercase().contains("BLOCKED"), "{:?}", r[0]);
        // cp-5 (§5.4): the operator's answer path needs the request_id, and the
        // only socket surface that carries it is this stream frame. Render it.
        let line = &r[0].line;
        assert!(
            line.contains("r1"),
            "the BLOCKED line must render the request_id: {line}"
        );
        assert!(
            line.contains("camp decide"),
            "the BLOCKED line must name the answer path: {line}"
        );
        assert!(
            !line.contains("cp-3"),
            "the BLOCKED line must not claim cp-3 is unshipped: {line}"
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

//! `camp attach <session>` (control-plane spec §5.2): the per-agent view. A
//! STATELESS RENDERER (§4.2): it opens a `session.subscribe` stream and renders
//! one worker's typed events live — tool calls with inputs, tool results,
//! assistant text, token usage. It never opens a session file, never learns a
//! pid; it reaches the worker only through the socket. Replay and live-follow are
//! the SAME subscribe (cursor 0 replays history then follows; a finished session
//! ends). From here you send a turn or interrupt; answering a permission request
//! drops in when cp-3's verb lands.

use anyhow::{Result, bail};

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
                vec![Rendered {
                    kind: EventKind::Permission,
                    line: format!(
                        "  !! BLOCKED -- {tool} needs your decision (answer lands when cp-3 ships)"
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use serde_json::json;

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

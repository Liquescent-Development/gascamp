//! cp-1 (control-plane spec §2, §2.1, §9): THE control wire.
//!
//! This module is the ONLY place in camp that constructs or parses a
//! `claude` control message. The format is undocumented, so it is pinned by
//! fixtures whose provenance is LABELLED (see
//! `tests/fixtures/control/PROVENANCE.md`): what was recorded from the real
//! CLI, what was derived from its shipped bundle, and what camp authored.
//! Concentrating the wire here is what makes those pins meaningful — a
//! second construction site elsewhere would not be covered by them.
//!
//! Two directions:
//!
//! - **Outbound** (`ParentMessage`) — what campd writes into a worker's
//!   held stdin: an `interrupt` control request, and the deterministic
//!   refusal of a `request_user_dialog` (§9).
//! - **Inbound** (`parse_worker_line`) — what campd reads back out of the
//!   worker's stdout file (cp-0's read channel tails it): a
//!   `control_response`, a `can_use_tool` request, a `request_user_dialog`
//!   request, or — the overwhelmingly common case — an ordinary stream line
//!   that camp passes through and never interprets.
//!
//! **D3, the surface rule.** The control surface is STRICT and the stream
//! surface is TRANSPARENT. Strictness keys on `type.starts_with("control")`,
//! so a future `control_notify` camp does not know becomes a LOUD fault
//! rather than content forwarded to a subscriber as if it were the worker's
//! own words. Everything that is not `control*` passes through verbatim.
#![allow(dead_code)] // cp-1: first read in Task 6 — DELETE this attribute there

use serde::{Deserialize, Serialize};

/// Every request id camp mints carries this prefix. It is what lets campd
/// tell its OWN request apart from one the CLI minted for itself (a
/// `can_use_tool` id, say) in a single glance at a ledger.
pub const REQUEST_ID_PREFIX: &str = "camp-";

/// A fresh control request id: `camp-<uuid-v4>`. Uniqueness is what makes
/// the pending table, the ledger correlation and a retrying caller's
/// de-duplication all work; a counter would collide across a restart.
pub fn new_request_id() -> String {
    format!("{REQUEST_ID_PREFIX}{}", uuid::Uuid::new_v4())
}

// ---------------------------------------------------------------------------
// OUTBOUND — what campd writes into a worker's held stdin.
//
// These are `#[derive(Serialize)]` STRUCTS, not `serde_json::json!` calls,
// and that is load-bearing: a struct serializes in DECLARATION order, while
// `json!` builds a `Map` that serde_json (no `preserve_order`) emits
// ALPHABETICALLY. The fixtures pin the bytes, so the two must not disagree.
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct InterruptEnvelope<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    request_id: &'a str,
    request: InterruptBody,
}

#[derive(Serialize)]
struct InterruptBody {
    subtype: &'static str,
}

#[derive(Serialize)]
struct ErrorResponseEnvelope<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    response: ErrorResponseBody<'a>,
}

#[derive(Serialize)]
struct ErrorResponseBody<'a> {
    subtype: &'static str,
    request_id: &'a str,
    error: &'a str,
}

/// A message campd sends INTO a worker's stdin.
pub enum ParentMessage {
    /// §4.1 `session.interrupt`. The CLI acks it with a `control_response`
    /// on its stdout, which cp-0's read channel tails back to campd.
    Interrupt { request_id: String },
    /// §9: camp is not a human and has no dialog to show. Every
    /// `request_user_dialog` gets this DETERMINISTIC refusal — because the
    /// alternative is a worker blocked forever waiting for an answer that
    /// can never come, holding a dispatch slot.
    DialogRefusal { request_id: String },
}

impl ParentMessage {
    /// The exact newline-terminated line to write into the pipe.
    ///
    /// `Result`, not a panic: this is library code and clippy denies
    /// `unwrap`/`expect`/`panic` here (invariant 5).
    pub fn to_line(&self) -> anyhow::Result<String> {
        let mut line = match self {
            ParentMessage::Interrupt { request_id } => serde_json::to_string(&InterruptEnvelope {
                kind: "control_request",
                request_id,
                request: InterruptBody {
                    subtype: "interrupt",
                },
            })?,
            ParentMessage::DialogRefusal { request_id } => {
                serde_json::to_string(&ErrorResponseEnvelope {
                    kind: "control_response",
                    response: ErrorResponseBody {
                        subtype: "error",
                        request_id,
                        error: "camp does not support interactive dialogs",
                    },
                })?
            }
        };
        line.push('\n');
        Ok(line)
    }
}

// ---------------------------------------------------------------------------
// INBOUND — what campd reads out of a worker's stdout file.
// ---------------------------------------------------------------------------

/// A line camp could not make sense of, carrying the line itself so the
/// fault event can name it (§2.1: loud, never swallowed).
#[derive(Debug)]
pub struct ControlWireError {
    pub line: String,
    pub reason: String,
}

/// One parsed line from a worker's stdout.
#[derive(Debug)]
pub enum WorkerMessage<'a> {
    /// The worker answered one of camp's control requests.
    ControlResponse {
        request_id: String,
        ok: bool,
        /// The `error` string on a failure; the inner `response` object,
        /// serialized, on a success. Diagnostic — camp never routes on it.
        detail: String,
    },
    /// §5.3.1: the CLI is asking permission to run a tool. Structurally
    /// UNREACHABLE in cp-1 (camp does not pass `--permission-prompt-tool`),
    /// so its arrival is a loud fault, and phase 3 owns the answer.
    CanUseTool {
        request_id: String,
        tool_name: String,
    },
    /// §9: the CLI wants to show a human a dialog. camp refuses, every time.
    RequestUserDialog { request_id: String },
    /// D3: everything that is not `control*`. camp passes it through and
    /// never interprets it.
    Stream(
        #[allow(dead_code)]
        // PERMANENT: never read in production. Subscribers are fed from the
        // FILE by pump (D6"), not from this variant — it exists so
        // parse_worker_line is TOTAL (D3's transparent surface) and so the
        // passthrough test can assert the bytes are unchanged.
        &'a str,
    ),
}

/// The permissive envelope. **Deliberately NOT `deny_unknown_fields`** (C9):
/// the peer is a minified bundle whose full key set cannot be proven by any
/// grep, and a parse that breaks when the CLI adds a key is a parse that
/// breaks in production. camp reads the keys it needs and ignores the rest.
#[derive(Deserialize)]
struct Envelope {
    #[serde(rename = "type")]
    kind: String,
    request_id: Option<String>,
    request: Option<serde_json::Value>,
    response: Option<serde_json::Value>,
}

fn wire_err(line: &str, reason: impl Into<String>) -> ControlWireError {
    ControlWireError {
        line: line.to_owned(),
        reason: reason.into(),
    }
}

/// Parse one complete stdout line. TOTAL: every line is either a control
/// message camp understands, an ordinary stream line, or a LOUD error.
///
/// D3's prefix rule lives here: strictness keys on `type.starts_with(
/// "control")`, never on an exhaustive list of known control types. A
/// `control_notify` the CLI adds tomorrow therefore FAULTS instead of being
/// forwarded to a subscriber as if it were the worker's own output.
pub fn parse_worker_line(line: &str) -> Result<WorkerMessage<'_>, ControlWireError> {
    let envelope: Envelope = match serde_json::from_str(line) {
        Ok(e) => e,
        Err(e) => {
            // A non-JSON line reaching HERE is a fault. (cp-0's `drain_one`
            // only hands over lines it already parsed, so in production this
            // arm is reached by a line that is valid JSON but not an object
            // with a `type` — belt and braces.)
            return Err(wire_err(line, format!("not a control envelope: {e}")));
        }
    };

    // D3: the transparent stream surface. The overwhelming majority of lines
    // land here, and camp never looks inside them.
    if !envelope.kind.starts_with("control") {
        return Ok(WorkerMessage::Stream(line));
    }

    match envelope.kind.as_str() {
        "control_response" => {
            let body = envelope
                .response
                .ok_or_else(|| wire_err(line, "a control_response with no `response` object"))?;
            // VERIFIED NESTING: the id is INSIDE `response`, not at the top
            // level (the bundle: `response:{subtype:"error",request_id:…}`).
            let request_id = body["request_id"]
                .as_str()
                .ok_or_else(|| wire_err(line, "a control_response with no `response.request_id`"))?
                .to_owned();
            match body["subtype"].as_str() {
                Some("success") => Ok(WorkerMessage::ControlResponse {
                    request_id,
                    ok: true,
                    detail: body["response"].to_string(),
                }),
                Some("error") => Ok(WorkerMessage::ControlResponse {
                    request_id,
                    ok: false,
                    // The `error` KEY is the verified one. The fallback is
                    // reachable only if the CLI stops sending it — in which
                    // case the fixture test is already RED and telling us so.
                    detail: body["error"]
                        .as_str()
                        .unwrap_or("the CLI reported an error but named no reason")
                        .to_owned(),
                }),
                other => Err(wire_err(
                    line,
                    format!("a control_response with an unknown subtype {other:?}"),
                )),
            }
        }
        "control_request" => {
            let body = envelope
                .request
                .ok_or_else(|| wire_err(line, "a control_request with no `request` object"))?;
            // The id is at the TOP level for a request (verified in the
            // bundle: `type==="control_request"&&"request_id" in e`).
            let request_id = envelope
                .request_id
                .ok_or_else(|| wire_err(line, "a control_request with no `request_id`"))?;
            match body["subtype"].as_str() {
                Some("can_use_tool") => Ok(WorkerMessage::CanUseTool {
                    request_id,
                    tool_name: body["tool_name"].as_str().unwrap_or_default().to_owned(),
                }),
                // `dialog_kind`'s VALUE SET was not recoverable from the
                // bundle (it is a minified variable), so camp NEVER keys on
                // it. It reads the id and refuses. That is a choice that
                // cannot rot.
                Some("request_user_dialog") => Ok(WorkerMessage::RequestUserDialog { request_id }),
                other => Err(wire_err(
                    line,
                    format!("a control_request with an unknown subtype {other:?}"),
                )),
            }
        }
        // D3's PREFIX RULE. Not an oversight — the whole point.
        other => Err(wire_err(
            line,
            format!(
                "unknown control message type {other:?}. camp refuses to guess at a control \
                 message it does not know: forwarding it to a subscriber would present the \
                 CLI's protocol chatter as the worker's own output (§2.1)"
            ),
        )),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    const INTERRUPT_REQUEST: &str =
        include_str!("../../tests/fixtures/control/interrupt_request.json");
    const CONTROL_RESPONSE_SUCCESS: &str =
        include_str!("../../tests/fixtures/control/control_response_success.json");
    const CONTROL_RESPONSE_ERROR: &str =
        include_str!("../../tests/fixtures/control/control_response_error.json");
    const CAN_USE_TOOL_REQUEST: &str =
        include_str!("../../tests/fixtures/control/can_use_tool_request.json");
    const REQUEST_USER_DIALOG_REQUEST: &str =
        include_str!("../../tests/fixtures/control/request_user_dialog_request.json");
    const DIALOG_REFUSAL_RESPONSE: &str =
        include_str!("../../tests/fixtures/control/dialog_refusal_response.json");
    const PERMISSION_ALLOW_RESPONSE: &str =
        include_str!("../../tests/fixtures/control/permission_allow_response.json");
    const PERMISSION_DENY_RESPONSE: &str =
        include_str!("../../tests/fixtures/control/permission_deny_response.json");
    const USER_TURN: &str = include_str!("../../tests/fixtures/control/user_turn.json");
    const STREAM_ASSISTANT: &str =
        include_str!("../../tests/fixtures/control/stream_assistant.json");

    /// Every shape camp SENDS is byte-equal to its fixture.
    ///
    /// Byte equality — not `Value` equality — is the point. The peer is a
    /// minified JS bundle whose parser we do not control, and camp's own
    /// `send_turn` bytes have shipped since Phase 8. `ParentMessage` is
    /// therefore built from `#[derive(Serialize)]` STRUCTS, whose field order
    /// is DECLARATION order, and never from `serde_json::json!`, whose key
    /// order is a `BTreeMap`'s (alphabetical).
    ///
    /// `user_turn.json` looks odd — `{"message":{...},"type":"user"}` — and
    /// that is CORRECT: `spawn::user_message` uses `json!`, so serde_json
    /// (1.0.150, no `preserve_order`) sorts its keys. Those are the bytes
    /// every production dispatch has always sent and the CLI has always
    /// accepted (probe P2). The fixture pins the ACTUAL output, so a later
    /// "tidy-up" of `user_message` into a struct — which would change the
    /// wire — turns this test RED. That is exactly what it is for.
    #[test]
    fn parent_messages_serialize_to_the_pinned_fixture_bytes() {
        let interrupt = ParentMessage::Interrupt {
            request_id: "camp-fixture-1".into(),
        }
        .to_line()
        .unwrap();
        assert_eq!(
            interrupt,
            format!("{INTERRUPT_REQUEST}\n"),
            "the interrupt camp sends must be byte-equal to its fixture"
        );

        let refusal = ParentMessage::DialogRefusal {
            request_id: "cli-fixture-3".into(),
        }
        .to_line()
        .unwrap();
        assert_eq!(
            refusal,
            format!("{DIALOG_REFUSAL_RESPONSE}\n"),
            "the dialog refusal camp sends must be byte-equal to its fixture"
        );

        // C1: the fixture is the ACTUAL output of the shipped code path.
        assert_eq!(
            crate::daemon::spawn::user_message("status?"),
            format!("{USER_TURN}\n"),
            "spawn::user_message's bytes are pinned — do not 'tidy' it into a struct"
        );
    }

    /// The order-independent guard: byte equality above could be satisfied by
    /// a fixture that is semantically wrong. This asserts the SHAPE too.
    #[test]
    fn parent_messages_are_semantically_equal_to_their_fixtures() {
        let interrupt = ParentMessage::Interrupt {
            request_id: "camp-fixture-1".into(),
        }
        .to_line()
        .unwrap();
        let sent: serde_json::Value = serde_json::from_str(&interrupt).unwrap();
        let pinned: serde_json::Value = serde_json::from_str(INTERRUPT_REQUEST).unwrap();
        assert_eq!(sent, pinned);
        assert_eq!(sent["type"], "control_request");
        assert_eq!(sent["request"]["subtype"], "interrupt");

        let refusal = ParentMessage::DialogRefusal {
            request_id: "cli-fixture-3".into(),
        }
        .to_line()
        .unwrap();
        let sent: serde_json::Value = serde_json::from_str(&refusal).unwrap();
        let pinned: serde_json::Value = serde_json::from_str(DIALOG_REFUSAL_RESPONSE).unwrap();
        assert_eq!(sent, pinned);
        assert_eq!(sent["response"]["subtype"], "error");
    }

    /// All four inbound shapes parse from the pinned fixtures.
    #[test]
    fn worker_messages_parse_from_the_pinned_fixtures() {
        match parse_worker_line(CONTROL_RESPONSE_SUCCESS).unwrap() {
            WorkerMessage::ControlResponse {
                request_id,
                ok,
                detail,
            } => {
                assert_eq!(request_id, "camp-fixture-1");
                assert!(ok);
                // The success detail is the inner `response` object verbatim.
                assert!(detail.contains("still_queued"), "detail was {detail:?}");
            }
            other => panic!("expected a ControlResponse, got {other:?}"),
        }

        match parse_worker_line(CONTROL_RESPONSE_ERROR).unwrap() {
            WorkerMessage::ControlResponse {
                request_id,
                ok,
                detail,
            } => {
                assert_eq!(request_id, "camp-fixture-1");
                assert!(!ok);
                // The `error` KEY is the verified one (recovered from the
                // bundle: `response:{subtype:"error",request_id:…,error:…}`).
                assert_eq!(detail, "no turn in progress");
            }
            other => panic!("expected a ControlResponse, got {other:?}"),
        }

        match parse_worker_line(CAN_USE_TOOL_REQUEST).unwrap() {
            WorkerMessage::CanUseTool {
                request_id,
                tool_name,
            } => {
                assert_eq!(request_id, "cli-fixture-2");
                assert_eq!(tool_name, "Bash");
            }
            other => panic!("expected a CanUseTool, got {other:?}"),
        }

        match parse_worker_line(REQUEST_USER_DIALOG_REQUEST).unwrap() {
            WorkerMessage::RequestUserDialog { request_id } => {
                assert_eq!(request_id, "cli-fixture-3");
            }
            other => panic!("expected a RequestUserDialog, got {other:?}"),
        }
    }

    /// C9: camp may NEVER depend on its `can_use_tool` fixture being
    /// COMPLETE. A fixed-width grep of a minified bundle cannot prove key
    /// completeness — and a second construction site in the very same bundle
    /// adds four keys the fixture does not carry. So the envelope is
    /// deliberately NOT `deny_unknown_fields`, and this test is what stops
    /// someone "tightening" it later.
    #[test]
    fn can_use_tool_with_unknown_extra_keys_still_parses() {
        let line = r#"{"type":"control_request","request_id":"cli-fixture-2","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{},"permission_suggestions":["allow"],"blocked_path":"/etc","decision_reason":{"type":"rule"},"decision_reason_type":"rule","classifier_approvable":true,"agent_id":"a1","future_key":"a key that does not exist yet"}}"#;
        match parse_worker_line(line).unwrap() {
            WorkerMessage::CanUseTool {
                request_id,
                tool_name,
            } => {
                assert_eq!(request_id, "cli-fixture-2");
                assert_eq!(tool_name, "Bash");
            }
            other => panic!("unknown keys must not break the parse; got {other:?}"),
        }
    }

    /// D3: the transparent stream surface. A non-control line is handed back
    /// VERBATIM and never faults — camp does not interpret the worker's
    /// words, it only routes them.
    #[test]
    fn non_control_stream_lines_pass_through_verbatim_and_never_fault() {
        for line in [
            STREAM_ASSISTANT,
            r#"{"type":"system","subtype":"init"}"#,
            r#"{"type":"result","subtype":"success","is_error":false}"#,
            r#"{"type":"user","message":{"role":"user","content":"hi"}}"#,
        ] {
            match parse_worker_line(line).unwrap() {
                WorkerMessage::Stream(passed) => assert_eq!(
                    passed, line,
                    "a stream line must pass through byte-for-byte"
                ),
                other => panic!("{line} must be a Stream, got {other:?}"),
            }
        }
    }

    /// §2.1: "an unrecognized control message … is an evented,
    /// operator-visible fault — never a swallowed timeout."
    ///
    /// The last case is D3's PREFIX rule and it is the load-bearing one: a
    /// `control_notify` does not exist today. If the CLI ever adds one, camp
    /// must FAULT on it — not forward its contents to a subscriber as though
    /// the worker had said them.
    #[test]
    fn an_unrecognized_control_message_is_a_loud_error() {
        for line in [
            // an unknown control_request subtype
            r#"{"type":"control_request","request_id":"x","request":{"subtype":"set_model"}}"#,
            // an unknown control_response subtype
            r#"{"type":"control_response","response":{"subtype":"weird","request_id":"x"}}"#,
            // a control_request with no request_id
            r#"{"type":"control_request","request":{"subtype":"interrupt"}}"#,
            // a control_response with no request_id
            r#"{"type":"control_response","response":{"subtype":"success"}}"#,
            // not JSON at all
            "this is not json",
            // a control message camp has never heard of
            r#"{"type":"control_cancel_request","request_id":"x"}"#,
            // THE PREFIX RULE: a type that does not exist yet
            r#"{"type":"control_notify","request_id":"x","note":"anything"}"#,
        ] {
            let err = parse_worker_line(line)
                .expect_err("an unrecognized control message must be a loud error");
            assert_eq!(err.line, line, "the error must carry the offending line");
            assert!(!err.reason.is_empty(), "the error must name a reason");
        }
    }

    /// C10: cp-3's OUTBOUND permission bytes, pinned HERE so they cannot rot
    /// before cp-3 arrives. cp-1 does not send them — but it does hand phase
    /// 3 recovered bytes instead of a guess, and the CLI's own validator
    /// string is the contract they are checked against:
    ///
    ///   Expected {behavior: 'allow', updatedInput?: object}
    ///         or {behavior: 'deny', message: string}.
    #[test]
    fn the_permission_response_fixtures_match_the_cli_validator_contract() {
        for (fixture, expected_behavior) in [
            (PERMISSION_ALLOW_RESPONSE, "allow"),
            (PERMISSION_DENY_RESPONSE, "deny"),
        ] {
            let v: serde_json::Value = serde_json::from_str(fixture).unwrap();
            assert_eq!(v["type"], "control_response");
            assert_eq!(v["response"]["subtype"], "success");
            assert!(v["response"]["request_id"].is_string());
            let decision = &v["response"]["response"];
            match expected_behavior {
                "allow" => {
                    assert_eq!(decision["behavior"], "allow");
                    // `updatedInput` is OPTIONAL per the validator.
                    assert!(
                        decision.get("updatedInput").is_none_or(|u| u.is_object()),
                        "updatedInput, when present, must be an object"
                    );
                }
                _ => {
                    assert_eq!(decision["behavior"], "deny");
                    assert!(
                        decision["message"].is_string(),
                        "a deny MUST carry a `message` string"
                    );
                }
            }
        }
    }
}

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The $0 tier of the real-`claude` compatibility gate (control-plane spec
//! §8 "the $0 tier", phase 0). A `#!/bin/sh` fake worker ignores argv and can
//! never reject a flag the real CLI rejects — that is exactly how #86 shipped
//! green. This gate spawns the REAL `claude` and asserts the contract camp
//! does not control: the HeldStream argv is accepted, the `initialize`
//! handshake round-trips, and a pre-turn `interrupt` is acknowledged.
//!
//! It costs $0 and needs no auth: every assertion happens BEFORE any turn is
//! sent (argv validation and the control protocol are CLI-local — verified:
//! the `initialize` response carries `account.tokenSource:"none"`). The gate
//! runs under a FRESH throwaway `CLAUDE_CONFIG_DIR`, which (a) keeps it
//! hermetic and (b) makes `verbose` default to `false`, so the negative
//! control honestly proves `--verbose` is required rather than being masked
//! by an operator's `"verbose": true` setting (the #86 contamination).
//!
//! LOCAL-ONLY and OPERATOR-GATED: the measured test `claude_compat_zero_cost`
//! is `#[ignore]`d AND requires `CAMP_COMPAT=1` (run via `make compat`); CI
//! compiles this file and runs ONLY the non-ignored pure tests below.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// The pinned `claude` version (Task 2). A build without the pin file fails to
/// compile — the pin is not optional.
const PINNED_VERSION: &str = include_str!("../../../ci/claude-compat/CLAUDE_VERSION");

/// The HeldStream worker flag list, mirroring `spawn.rs::build_spec`'s
/// `StdinMode::HeldStream` arm (its exact output is pinned by that module's
/// `stream_argv_matches_probe_p2_and_the_fixture_facts` unit test — keep the
/// two in sync). `with_verbose=false` reproduces the PRE-FIX argv for the
/// negative control that reproduces #86.
fn held_stream_flags(session_id: &str, with_verbose: bool) -> Vec<String> {
    let mut v = vec![
        "-p".to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
    ];
    if with_verbose {
        v.push("--verbose".to_string());
    }
    v.push("--input-format".to_string());
    v.push("stream-json".to_string());
    v.push("--session-id".to_string());
    v.push(session_id.to_string());
    v
}

/// True iff `line` is a `control_response` with `subtype=="success"` and the
/// given `request_id`. Pins the wire shape (spec §2.1). A malformed or
/// non-matching line is simply `false` (the gate keeps reading until it finds
/// the match or times out).
fn control_response_is_success(line: &str, request_id: &str) -> bool {
    let v: serde_json::Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return false,
    };
    v["type"] == "control_response"
        && v["response"]["subtype"] == "success"
        && v["response"]["request_id"] == request_id
}

#[test]
fn pinned_version_file_is_present_and_shaped() {
    let pin = PINNED_VERSION.trim();
    assert!(!pin.is_empty(), "CLAUDE_VERSION pin must not be empty");
    assert!(
        !pin.contains(char::is_whitespace),
        "pin must be a bare version token (no `(Claude Code)` suffix, no spaces): {pin:?}"
    );
    // Shape: dotted numeric, e.g. 2.1.207.
    assert!(
        pin.split('.').all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())),
        "pin must be a dotted numeric version: {pin:?}"
    );
}

#[test]
fn held_stream_flags_include_verbose_in_sdk_order() {
    // The fixed argv (cross-check of the Task 1 fix): --verbose sits between
    // the stream-json output value and --input-format, matching the SDK.
    assert_eq!(
        held_stream_flags("sid-1", true),
        vec![
            "-p",
            "--output-format",
            "stream-json",
            "--verbose",
            "--input-format",
            "stream-json",
            "--session-id",
            "sid-1",
        ]
    );
    // The pre-fix argv (negative control): identical but for --verbose.
    assert_eq!(
        held_stream_flags("sid-1", false),
        vec![
            "-p",
            "--output-format",
            "stream-json",
            "--input-format",
            "stream-json",
            "--session-id",
            "sid-1",
        ]
    );
}

#[test]
fn control_response_parser_pins_the_wire_shape() {
    // Recorded verbatim from claude 2.1.207 (pre-turn interrupt ack, $0 run).
    let ok = r#"{"type":"control_response","response":{"subtype":"success","request_id":"req_int","response":{"still_queued":[]}}}"#;
    assert!(control_response_is_success(ok, "req_int"));
    // Wrong request_id must not match.
    assert!(!control_response_is_success(ok, "other"));
    // An error subtype is not success.
    let err = r#"{"type":"control_response","response":{"subtype":"error","request_id":"req_int","response":{}}}"#;
    assert!(!control_response_is_success(err, "req_int"));
    // A non-control line is not a match.
    let other = r#"{"type":"system","subtype":"init"}"#;
    assert!(!control_response_is_success(other, "req_int"));
    // Garbage is not a match (parser must not panic).
    assert!(!control_response_is_success("not json", "req_int"));
}

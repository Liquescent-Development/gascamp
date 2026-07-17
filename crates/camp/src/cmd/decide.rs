//! `camp decide <session> <request_id> allow|allow_always|deny [--reason ...]`
//! (control-plane §5.3.4/§9): answer a worker's `can_use_tool`. `camp watch`
//! shows the BLOCKED row and its `request_id`; this verb records the operator's
//! decision and delivers it to the worker.
//!
//! A PURE CLIENT (design §4.3): it never starts campd. The decision must reach
//! the live worker's held stdin, which only the running daemon holds — so a
//! campd that is down is a loud, actionable error, never a silent no-op.

use anyhow::{Result, bail};

use crate::campdir::CampDir;
use crate::daemon::socket::{self, Request, Response};

/// Build a VALIDATED permission answer — the ONE place the operator-facing rules
/// of a decision live: the decision vocabulary, and deny-requires-a-reason.
///
/// Shared with `camp attach`'s in-view `/allow`//deny line (issue #120), which
/// answers over the same socket verb. A second copy of these rules in the attach
/// loop is exactly the drift this repo has been bitten by: the two surfaces would
/// silently disagree about what a valid answer is. campd validates independently
/// (`control.rs::serve_permission_decision`) — that is the belt to this braces,
/// not a reason to skip validating here: a typo should be a client error, not a
/// round-trip.
pub fn decision_request(
    session: String,
    request_id: String,
    decision: String,
    reason: Option<String>,
) -> Result<Request> {
    if !matches!(decision.as_str(), "allow" | "allow_always" | "deny") {
        bail!("unknown decision {decision:?} — one of allow | allow_always | deny");
    }
    if decision == "deny" && reason.as_deref().map(str::trim).unwrap_or("").is_empty() {
        bail!("a `deny` decision must carry a reason (the operator's message the worker sees)");
    }
    Ok(Request::SessionPermissionDecision {
        session,
        request_id,
        decision,
        message: reason,
    })
}

pub fn run(
    camp: &CampDir,
    session: String,
    request_id: String,
    decision: String,
    reason: Option<String>,
) -> Result<()> {
    let request = decision_request(session.clone(), request_id.clone(), decision, reason)?;
    let response = socket::require(camp, &request)?;
    match response {
        Response::PermissionDecided {
            decision: recorded, ..
        } => {
            println!(
                "recorded {recorded} for {request_id} on {session}, and delivered it to the worker"
            );
            Ok(())
        }
        Response::Error { error, .. } => bail!("{error}"),
        other => bail!("unexpected response to the permission decision: {other:?}"),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn req(decision: &str, reason: Option<&str>) -> Result<Request> {
        decision_request(
            "t/dev/1".into(),
            "cli-2".into(),
            decision.into(),
            reason.map(str::to_owned),
        )
    }

    /// The vocabulary is closed: three decisions, and a typo is a CLIENT error —
    /// never a round-trip, and never passed through for campd to puzzle over.
    #[test]
    fn only_the_three_decisions_are_answers() {
        assert!(req("allow", None).is_ok());
        assert!(req("allow_always", None).is_ok());
        assert!(req("deny", Some("not on prod")).is_ok());
        for typo in ["Allow", "yes", "allow-always", "denied", ""] {
            let e = req(typo, Some("r")).expect_err("{typo} must not be an answer");
            assert!(
                e.to_string().contains("allow | allow_always | deny"),
                "the error must name the vocabulary: {e}"
            );
        }
    }

    /// §5.3: a deny is the operator's MESSAGE TO THE WORKER — a deny with nothing
    /// to say tells the worker only that it lost. This rule lives here ONCE and
    /// both surfaces (`camp decide --reason`, attach's `/deny <reason>`) inherit
    /// it; whitespace is not a reason.
    #[test]
    fn a_deny_must_carry_a_reason_and_blank_is_not_one() {
        for empty in [None, Some(""), Some("   "), Some("\t\n")] {
            let e = req("deny", empty).expect_err("a reasonless deny must be refused");
            assert!(
                e.to_string().contains("reason"),
                "the error must name the missing reason: {e}"
            );
        }
        assert!(req("deny", Some("not on prod")).is_ok());
    }

    /// An ALLOW needs no reason: only a deny owes the worker an explanation.
    /// Demanding one everywhere would make attach's bare `/allow` — #120's whole
    /// point — impossible.
    #[test]
    fn an_allow_needs_no_reason() {
        assert!(req("allow", None).is_ok());
        assert!(req("allow_always", None).is_ok());
    }

    /// The validated output is the WIRE REQUEST itself, carrying every field
    /// through unaltered — the validator cannot be bypassed by building a
    /// `SessionPermissionDecision` some other way and skipping it.
    #[test]
    fn the_validated_answer_is_the_wire_request_verbatim() {
        let r = req("deny", Some("  not on prod  ")).unwrap();
        assert_eq!(
            r,
            Request::SessionPermissionDecision {
                session: "t/dev/1".into(),
                request_id: "cli-2".into(),
                decision: "deny".into(),
                // The reason reaches the WORKER as the operator typed it: trimming
                // is how the rule JUDGES a reason, not a rewrite of their words.
                message: Some("  not on prod  ".into()),
            }
        );
    }
}

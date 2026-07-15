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

pub fn run(
    camp: &CampDir,
    session: String,
    request_id: String,
    decision: String,
    reason: Option<String>,
) -> Result<()> {
    // Fail fast on an unknown decision here too, so a typo is a client error
    // rather than a round-trip (campd validates it again — belt and braces).
    if !matches!(decision.as_str(), "allow" | "allow_always" | "deny") {
        bail!("unknown decision {decision:?} — one of allow | allow_always | deny");
    }
    if decision == "deny" && reason.as_deref().map(str::trim).unwrap_or("").is_empty() {
        bail!("a `deny` decision must carry --reason (the operator's message the worker sees)");
    }

    let response = socket::require(
        camp,
        &Request::SessionPermissionDecision {
            session: session.clone(),
            request_id: request_id.clone(),
            decision: decision.clone(),
            message: reason,
        },
    )?;
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

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

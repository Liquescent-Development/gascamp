use anyhow::{Result, bail};

use crate::campdir::CampDir;
use crate::daemon::socket::{self, Request, Response};

/// `camp stop`: graceful daemon shutdown over the socket. Never
/// auto-starts (stopping nothing is an error, not a no-op).
pub fn run(camp: &CampDir) -> Result<()> {
    // A wedge is not "not running" (issue #55): the CampdUnresponsive
    // error already carries the truth (pid + kill -9 remedy) — layering
    // "campd is not running" over it would misdiagnose a live-but-stuck
    // daemon as an absent one.
    let response = socket::request(camp, &Request::Stop).map_err(|e| {
        if e.downcast_ref::<socket::CampdUnresponsive>().is_some() {
            e
        } else {
            e.context("campd is not running")
        }
    })?;
    match response {
        Response::Ok { .. } => {
            println!("campd stopped");
            Ok(())
        }
        other => bail!("unexpected response to stop: {other:?}"),
    }
}

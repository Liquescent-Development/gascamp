use anyhow::{Context, Result, bail};

use crate::campdir::CampDir;
use crate::daemon::socket::{self, Request, Response};

/// `camp stop`: graceful daemon shutdown over the socket. Never
/// auto-starts (stopping nothing is an error, not a no-op).
pub fn run(camp: &CampDir) -> Result<()> {
    let response =
        socket::request(&camp.socket_path(), &Request::Stop).context("campd is not running")?;
    match response {
        Response::Ok { .. } => {
            println!("campd stopped");
            Ok(())
        }
        other => bail!("unexpected response to stop: {other:?}"),
    }
}

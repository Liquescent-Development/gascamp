use anyhow::{Result, bail};

use crate::campdir::CampDir;
use crate::daemon::autostart;
use crate::daemon::socket::{Request, Response};

/// `camp adopt`: reconcile the session registry against reality (spec
/// §8.5) — the routine campd runs automatically at start, on demand.
/// Auto-starts campd when it is down (the fresh daemon adopts at startup;
/// the explicit request that follows is a no-op by construction —
/// adoption is idempotent).
pub fn run(camp: &CampDir) -> Result<()> {
    let response = autostart::request_with_autostart(camp, &Request::Adopt, "adopt")?;
    match response {
        Response::Adopt {
            crashed,
            rearmed,
            released,
            swept,
            kept,
            ..
        } => {
            println!(
                "adopted: {crashed} crashed, {rearmed} re-armed, {released} released, \
                 {swept} worktrees swept, {kept} kept"
            );
            Ok(())
        }
        other => bail!("unexpected response to adopt: {other:?}"),
    }
}

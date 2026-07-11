use anyhow::{Result, bail};

use crate::campdir::CampDir;
use crate::daemon::socket::{self, Request, Response};

/// `camp adopt`: reconcile the session registry against reality (spec §8.5) —
/// the routine campd runs automatically at start, on demand. A PURE CLIENT
/// (design §4.3): campd holds the registry and the timers, so this verb needs
/// it; a campd that is down is a loud, actionable error, never a spawn.
pub fn run(camp: &CampDir) -> Result<()> {
    let response = socket::require(camp, &Request::Adopt)?;
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

//! `camp sessions [--json]` (control-plane §5.4): the overseer's one-shot
//! snapshot of the live fleet — one row per session, addressed BY NAME (§4.2),
//! sourced ONLY from the socket's `sessions.list` verb. The non-streaming
//! sibling of `camp watch`: `watch` is the human's live second-monitor view;
//! `sessions` is the snapshot an AGENT overseer reads once and moves on from.
//!
//! A PURE CLIENT (design §4): it reaches the fleet ONLY through the socket. It
//! never opens a worker's stdout stream file under `sessions/`, never reads a
//! pid — a down campd is a loud, actionable error (the socket verb's own
//! `CampdNotRunning`), never a silent read of on-disk state.

use anyhow::{Result, bail};

use crate::campdir::CampDir;
use crate::daemon::socket::{self, Request, Response, SessionInfo};

pub fn run(camp: &CampDir, json: bool) -> Result<()> {
    // A PURE CLIENT (design §4.3, mirror of `camp decide`): it never starts
    // campd, and a down campd is a loud error — the fleet lives behind the
    // socket, never in a file this client is allowed to read.
    let response = socket::require(camp, &Request::SessionsList)?;
    match response {
        Response::SessionsList { sessions, .. } => {
            if json {
                // The machine read (operator skill's `--json` discipline): the
                // exact wire `SessionInfo` vec, verbatim.
                println!("{}", serde_json::to_string(&sessions)?);
            } else {
                print!("{}", render(&sessions));
            }
            Ok(())
        }
        Response::Error { error, .. } => bail!("{error}"),
        other => bail!("unexpected response to sessions.list: {other:?}"),
    }
}

/// One line per session: `NAME  AGENT  RIG  BEAD  STATE`, where a BLOCKED
/// session (§5.3) renders `BLOCKED` in the STATE column — the state that
/// matters and that must be impossible to miss (§5.1).
fn render(sessions: &[SessionInfo]) -> String {
    if sessions.is_empty() {
        return "no live sessions\n".to_owned();
    }
    let mut out = String::from("NAME                 AGENT            RIG        BEAD          STATE\n");
    for s in sessions {
        let state = if s.blocked { "BLOCKED" } else { s.state.as_str() };
        out.push_str(&format!(
            "{:<20} {:<16} {:<10} {:<13} {}\n",
            s.name,
            s.agent,
            s.rig.as_deref().unwrap_or("-"),
            s.bead.as_deref().unwrap_or("-"),
            state,
        ));
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn info(name: &str, state: &str, blocked: bool) -> SessionInfo {
        SessionInfo {
            name: name.to_owned(),
            agent: "dev".to_owned(),
            rig: Some("t3".to_owned()),
            bead: Some("t3-1".to_owned()),
            state: state.to_owned(),
            last_activity: "2026-07-15T00:00:00Z".to_owned(),
            blocked,
        }
    }

    #[test]
    fn render_shows_one_row_per_session_and_surfaces_blocked() {
        let out = render(&[
            info("t3/dev/1", "working", false),
            info("t3/dev/2", "working", true),
            info("t3/dev/3", "stalled", false),
        ]);
        // one row per session, by name
        assert!(out.contains("t3/dev/1"));
        assert!(out.contains("t3/dev/2"));
        assert!(out.contains("t3/dev/3"));
        // BLOCKED overrides the working/stalled state and is spelled loudly
        assert!(out.contains("BLOCKED"), "blocked session must render BLOCKED: {out}");
        // the non-blocked states survive
        assert!(out.contains("working"));
        assert!(out.contains("stalled"));
    }

    #[test]
    fn render_of_an_empty_fleet_is_a_clear_single_line() {
        let out = render(&[]);
        assert!(out.to_lowercase().contains("no live session"), "got: {out}");
    }
}

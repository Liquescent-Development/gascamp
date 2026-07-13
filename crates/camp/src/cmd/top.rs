use anyhow::{Result, bail};
use camp_core::ledger::StatusSummary;

use crate::campdir::CampDir;
use crate::daemon::socket::{self, Request, Response};

/// `camp top`: ONE status query rendered as plain text — a query, not a loop
/// (spec §5); refresh is running it again. A PURE CLIENT (design §4.3): it
/// never starts campd — a campd that is down is a loud, actionable error.
pub fn run(camp: &CampDir) -> Result<()> {
    let response = socket::require(camp, &Request::Status)?;
    let Response::Status {
        summary,
        red,
        campd_pid,
        ..
    } = response
    else {
        bail!("unexpected response to status: {response:?}");
    };
    print!("{}", render(&summary, red, campd_pid));
    Ok(())
}

/// `camp top --statusline`: the compact fleet badge `▲live ●ready ✖red`, from
/// ONE read-only socket query. When campd is down it prints nothing to stdout
/// and writes a visible stderr note, exiting 0 — visible degradation, not
/// silence (spec §11). It is the one daemon-needing surface that does NOT fail
/// loudly, by design: a status line may never break the user's prompt. The
/// plugin's statusline snippet is a thin wrapper over this.
pub fn statusline(camp: &CampDir) -> Result<()> {
    match socket::request(camp, &Request::Status) {
        Ok(Response::Status { summary, red, .. }) => {
            println!(
                "▲{} ●{} ✖{}",
                summary.live_sessions.len(),
                summary.ready,
                red
            );
            Ok(())
        }
        Ok(other) => bail!("unexpected response to status: {other:?}"),
        // campd down or wedged: degrade visibly (stderr), never fail the
        // caller. The badge is empty; the note says why.
        Err(e) => {
            eprintln!("camp: campd unavailable — statusline empty ({e:#})");
            Ok(())
        }
    }
}

fn render(summary: &StatusSummary, red: u64, campd_pid: u32) -> String {
    let sessions = if summary.live_sessions.is_empty() {
        "0".to_owned()
    } else {
        format!(
            "{} ({})",
            summary.live_sessions.len(),
            summary.live_sessions.join(", ")
        )
    };
    format!(
        "campd pid: {campd_pid}\nlive sessions: {sessions}\nready: {}\nopen: {}\nstuck: {}\nred: {red}\n",
        summary.ready, summary.open, summary.stuck
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use camp_core::ledger::StatusSummary;

    #[test]
    fn render_is_plain_text_and_stable() {
        let empty = StatusSummary {
            live_sessions: vec![],
            ready: 0,
            open: 0,
            stuck: 0,
        };
        assert_eq!(
            render(&empty, 0, 4242),
            "campd pid: 4242\nlive sessions: 0\nready: 0\nopen: 0\nstuck: 0\nred: 0\n"
        );
        let busy = StatusSummary {
            live_sessions: vec!["camp/dev/1".to_owned(), "camp/dev/2".to_owned()],
            ready: 1,
            open: 3,
            stuck: 0,
        };
        assert_eq!(
            render(&busy, 1, 7),
            "campd pid: 7\nlive sessions: 2 (camp/dev/1, camp/dev/2)\nready: 1\nopen: 3\nstuck: 0\nred: 1\n"
        );
    }
}

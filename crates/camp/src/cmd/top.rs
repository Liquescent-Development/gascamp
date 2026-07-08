use anyhow::{Result, bail};
use camp_core::ledger::StatusSummary;

use crate::campdir::CampDir;
use crate::daemon::autostart;
use crate::daemon::socket::{Request, Response};

/// `camp top`: ONE status query rendered as plain text — a query, not a
/// loop (spec §5); refresh is running it again. Auto-starts campd.
pub fn run(camp: &CampDir) -> Result<()> {
    let response = autostart::request_with_autostart(camp, &Request::Status, "top")?;
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
        "campd pid: {campd_pid}\nlive sessions: {sessions}\nready: {}\nopen: {}\nred: {red}\n",
        summary.ready, summary.open
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
        };
        assert_eq!(
            render(&empty, 0, 4242),
            "campd pid: 4242\nlive sessions: 0\nready: 0\nopen: 0\nred: 0\n"
        );
        let busy = StatusSummary {
            live_sessions: vec!["camp/dev/1".to_owned(), "camp/dev/2".to_owned()],
            ready: 1,
            open: 3,
        };
        assert_eq!(
            render(&busy, 1, 7),
            "campd pid: 7\nlive sessions: 2 (camp/dev/1, camp/dev/2)\nready: 1\nopen: 3\nred: 1\n"
        );
    }
}

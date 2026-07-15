//! `camp watch` (control-plane spec §5.1): the fleet view — the thing you leave
//! open on a second monitor. A STATELESS RENDERER (§4.2): it opens a
//! `fleet.subscribe` stream and replaces its rows BY NAME as frames arrive. It
//! never tails a file, never reads the ledger, never learns a pid. Push-driven:
//! it blocks on the socket between updates — zero polling.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

use anyhow::{Result, bail};
use jiff::Timestamp;
use serde::Deserialize;

use crate::campdir::CampDir;
use crate::daemon::socket::{self, Request, Response, SessionInfo};

/// One frame off the `fleet.subscribe` wire. Lenient — the daemon may add frame
/// kinds a future phase understands; an unknown `frame` is ignored, never a
/// crash (the client is a renderer, not a validator of campd's own protocol).
#[derive(Deserialize)]
#[serde(tag = "frame", rename_all = "snake_case")]
enum Frame {
    Session {
        session: SessionInfo,
    },
    Gone {
        name: String,
    },
    Synced,
    #[serde(other)]
    Unknown,
}

pub fn run(camp: &CampDir) -> Result<()> {
    // The hello is bounded by REQUEST_TIMEOUT (a wedged campd fails fast, like
    // every verb); after it, the stream is timeout-exempt. A pure client never
    // starts campd — a down campd is the standard loud error.
    let path = camp.socket_path();
    let mut stream = match UnixStream::connect(&path) {
        Ok(s) => s,
        Err(_) => {
            socket::require(camp, &Request::FleetSubscribe)?; // returns Err(CampdNotRunning)
            return Ok(()); // unreachable — require errored — but keeps the type total
        }
    };
    stream.set_read_timeout(Some(socket::REQUEST_TIMEOUT))?;
    stream.set_write_timeout(Some(socket::REQUEST_TIMEOUT))?;
    let mut line = serde_json::to_string(&Request::FleetSubscribe)?;
    line.push('\n');
    stream.write_all(line.as_bytes())?;

    let mut reader = BufReader::new(stream);
    let mut hello = String::new();
    reader.read_line(&mut hello)?;
    match serde_json::from_str::<Response>(hello.trim_end()) {
        Ok(Response::FleetSubscribed { ok: true, .. }) => {}
        Ok(Response::Error { error, .. }) => bail!("campd refused fleet.subscribe: {error}"),
        other => bail!("unexpected fleet.subscribe hello: {other:?}"),
    }
    // Long-lived now: no read timeout (a quiet fleet is not a wedged daemon — §4.4).
    reader.get_ref().set_read_timeout(None)?;

    let mut rows: BTreeMap<String, SessionInfo> = BTreeMap::new();
    let mut state_since: BTreeMap<String, Timestamp> = BTreeMap::new();
    let mut synced = false;

    loop {
        let mut frame_line = String::new();
        let n = reader.read_line(&mut frame_line)?;
        if n == 0 {
            eprintln!("camp watch: campd closed the stream");
            return Ok(());
        }
        let trimmed = frame_line.trim_end();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<Frame>(trimmed) {
            Ok(Frame::Session { session }) => {
                let name = session.name.clone();
                let display = state_display(&session);
                let changed =
                    rows.get(&name).map(state_display).as_deref() != Some(display.as_str());
                if changed || !state_since.contains_key(&name) {
                    state_since.insert(name.clone(), Timestamp::now());
                }
                rows.insert(name, session);
            }
            Ok(Frame::Gone { name }) => {
                rows.remove(&name);
                state_since.remove(&name);
            }
            Ok(Frame::Synced) => synced = true,
            Ok(Frame::Unknown) => {}
            Err(e) => bail!("malformed fleet frame {trimmed:?}: {e}"),
        }
        if synced {
            print!("{}", render(&rows, &state_since, Timestamp::now()));
            std::io::stdout().flush().ok();
        }
    }
}

/// The STATE cell: BLOCKED (§5.3, rendered though cp-2 never produces it) wins;
/// else the working/stalled state verbatim.
fn state_display(s: &SessionInfo) -> String {
    if s.blocked {
        "BLOCKED".to_owned()
    } else {
        s.state.clone()
    }
}

fn fmt_dur(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    format!("{}m{:02}s", secs / 60, secs % 60)
}

/// Age of an RFC3339 timestamp from `now`, saturating at zero.
fn age(ts_str: &str, now: Timestamp) -> std::time::Duration {
    match ts_str.parse::<Timestamp>() {
        Ok(ts) => {
            let delta = now - ts;
            if delta.is_negative() {
                std::time::Duration::ZERO
            } else {
                std::time::Duration::try_from(delta).unwrap_or(std::time::Duration::ZERO)
            }
        }
        Err(_) => std::time::Duration::ZERO,
    }
}

/// Render the fleet: a header and one line per session, sorted by name (BTreeMap
/// order — a stable frame). Clears the screen first so refresh is in-place.
fn render(
    rows: &BTreeMap<String, SessionInfo>,
    state_since: &BTreeMap<String, Timestamp>,
    now: Timestamp,
) -> String {
    let mut out = String::new();
    out.push_str("\x1b[2J\x1b[H"); // clear + home
    out.push_str(&format!(
        "{:<18} {:<13} {:<10} {:>7}  {}\n",
        "AGENT", "BEAD", "STATE", "FOR", "LAST"
    ));
    for (name, s) in rows {
        let state = state_display(s);
        let for_str = state_since
            .get(name)
            .map(|since| {
                let d = now - *since;
                fmt_dur(if d.is_negative() {
                    std::time::Duration::ZERO
                } else {
                    std::time::Duration::try_from(d).unwrap_or(std::time::Duration::ZERO)
                })
            })
            .unwrap_or_else(|| "0m00s".to_owned());
        let last_age = age(&s.last_activity, now);
        // cp-2's LAST is a relative-time indicator (scoping decision 1): a
        // BLOCKED session says "needs you"; a stalled one "no output <age>";
        // else the age of the last line. The rich tool summary is phase 4.
        let last = if s.blocked {
            format!("? {} — needs you", s.bead.as_deref().unwrap_or(""))
        } else if s.state == "stalled" {
            format!("(no output {})", fmt_dur(last_age))
        } else {
            fmt_dur(last_age)
        };
        out.push_str(&format!(
            "{:<18} {:<13} {:<10} {:>7}  {}\n",
            s.agent,
            s.bead.as_deref().unwrap_or("-"),
            state,
            for_str,
            last
        ));
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::daemon::socket::SessionInfo;
    use std::collections::BTreeMap;

    fn row(
        name: &str,
        agent: &str,
        bead: &str,
        state: &str,
        blocked: bool,
        last: &str,
    ) -> SessionInfo {
        SessionInfo {
            name: name.into(),
            agent: agent.into(),
            rig: Some("gc".into()),
            bead: Some(bead.into()),
            state: state.into(),
            last_activity: last.into(),
            blocked,
        }
    }

    #[test]
    fn render_shows_a_header_and_one_line_per_session_with_blocked_and_stalled_columns() {
        let now: Timestamp = "2026-07-14T00:10:00Z".parse().unwrap();
        let mut rows = BTreeMap::new();
        rows.insert(
            "a".to_string(),
            row(
                "a",
                "bmad/dev",
                "campdemo-15",
                "working",
                true,
                "2026-07-14T00:09:29Z",
            ),
        );
        rows.insert(
            "b".to_string(),
            row(
                "b",
                "gstack/reviewer",
                "campdemo-12",
                "working",
                false,
                "2026-07-14T00:03:58Z",
            ),
        );
        rows.insert(
            "c".to_string(),
            row(
                "c",
                "bmad/dev",
                "campdemo-11",
                "stalled",
                false,
                "2026-07-13T23:58:00Z",
            ),
        );
        let mut since = BTreeMap::new();
        since.insert("a".to_string(), "2026-07-14T00:09:29Z".parse().unwrap());
        since.insert("b".to_string(), "2026-07-14T00:03:58Z".parse().unwrap());
        since.insert("c".to_string(), "2026-07-13T23:55:10Z".parse().unwrap());

        let out = render(&rows, &since, now);
        assert!(
            out.contains("AGENT")
                && out.contains("BEAD")
                && out.contains("STATE")
                && out.contains("FOR")
                && out.contains("LAST")
        );
        assert!(out.contains("BLOCKED"), "blocked row shows BLOCKED: {out}");
        assert!(
            out.contains("needs you"),
            "BLOCKED must be impossible to miss: {out}"
        );
        assert!(out.contains("stalled"), "{out}");
        assert!(out.contains("no output"), "{out}");
        assert!(
            out.contains("gstack/reviewer") && out.contains("campdemo-12"),
            "{out}"
        );
    }

    #[test]
    fn fmt_dur_is_minutes_and_zero_padded_seconds() {
        assert_eq!(fmt_dur(std::time::Duration::from_secs(134)), "2m14s");
        assert_eq!(fmt_dur(std::time::Duration::from_secs(31)), "0m31s");
        assert_eq!(fmt_dur(std::time::Duration::from_secs(362)), "6m02s");
    }
}

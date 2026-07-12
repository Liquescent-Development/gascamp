//! `camp nudge <session> "<text>"` (dispatch-lifecycle Phase 1, #29 — the
//! converse verb, mirror of `gc nudge`/session-message): send one user turn
//! to any registered session. Live path: campd holds the worker's stream
//! stdin (Decision C) — the turn lands in its CURRENT conversation now.
//! Resume path (worker exited, pipe released, attended session, campd
//! down): `<dispatch.command> -p --resume <sid> "<text>"` from the
//! session's recorded cwd — the turn lands after its last one (A4/F6) and
//! the reply prints here. Interactivity is a runtime/harness capability,
//! never a dispatch mode: this verb never dispatches, reserves, or spawns
//! workers, and it never starts campd (a fresh campd could hold no
//! pipe for the target anyway).

use std::path::PathBuf;
use std::process::Stdio;

use anyhow::{Context, Result, anyhow, bail};
use camp_core::config::CampConfig;
use camp_core::event::{EventInput, EventType};
use camp_core::ledger::{Ledger, SessionRow};

use crate::campdir::CampDir;
use crate::daemon::socket::{self, Request, Response};

pub fn run(camp: &CampDir, session: String, text: String) -> Result<()> {
    let config = CampConfig::load(&camp.config_path())?;
    let ledger = Ledger::open(&camp.db_path())?;
    let row = ledger.session_by_name(&session)?.ok_or_else(|| {
        anyhow!(
            "no session named {session:?} in the registry; `camp top` lists live \
             sessions, and for exited ones the name is in `camp show <bead>` / \
             `camp events` (the session.woke `name`)"
        )
    })?;
    drop(ledger); // the resume path re-opens for the append; campd may write meanwhile

    if row.status == "live" {
        // A down campd is a normal state for THIS verb — it never requires the
        // daemon — and a fresh campd holds no pipes anyway: Ok(None) routes to
        // resume (A4).
        match socket::request_if_up(
            camp,
            &Request::Nudge {
                session: session.clone(),
                text: text.clone(),
            },
        )? {
            Some(Response::Nudge { via, .. }) if via == "stdin" => {
                // Mechanism-honest (assessment findings A/B): the turn is
                // WRITTEN INTO the held pipe — the worker picks it up at
                // its next read, and its answer lands in its transcript
                // (`camp events` records only the session.nudged delivery).
                println!(
                    "wrote the turn into {session}'s held stdin (live); the worker \
                     picks it up at its next read — watch its transcript for the reply"
                );
                return Ok(());
            }
            Some(Response::Nudge { .. }) => {} // via="none": no pipe → resume
            Some(other) => bail!("unexpected response to nudge: {other:?}"),
            None => {} // campd down → resume
        }
    }
    resume(camp, &config, &row, &session, &text)
}

fn resume(
    camp: &CampDir,
    config: &CampConfig,
    row: &SessionRow,
    session: &str,
    text: &str,
) -> Result<()> {
    let sid = row.claude_session_id.as_deref().ok_or_else(|| {
        anyhow!("session {session:?} has no recorded claude session id; cannot resume it")
    })?;
    let cwd = resume_cwd(config, row)?;
    // Same argv vocabulary as patrol's nudge-resume (spawn::resume_argv):
    // the recorded F7 pins ride every resume turn (#48 finding 1).
    let pins = spawn_pins(row);
    let out = std::process::Command::new(&config.dispatch.command)
        .args(crate::daemon::spawn::resume_argv(sid, text, &pins))
        .current_dir(&cwd)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("running {} --resume", config.dispatch.command.display()))?;
    if !out.status.success() {
        bail!(
            "resume of {session} (claude session {sid}) failed (exit {:?}): {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let reply = parse_result_text(&out.stdout)?;
    // Durable record of the injected turn (invariant 3), appended only
    // after confirmed delivery.
    let mut ledger = Ledger::open(&camp.db_path())?;
    ledger.append(EventInput {
        kind: EventType::SessionNudged,
        rig: row.rig.clone(),
        actor: "cli".into(),
        bead: row.bead.clone(),
        data: serde_json::json!({ "session": session, "via": "resume", "text": text }),
    })?;
    println!("{reply}");
    Ok(())
}

/// The registry row's recorded F7 pins as spawn's resume vocabulary
/// (issue #48 finding 1): None fields = registered without pins = a bare
/// resume, a recorded absence.
fn spawn_pins(row: &SessionRow) -> crate::daemon::spawn::ResumePins {
    crate::daemon::spawn::ResumePins {
        model: row.model.clone(),
        permission_mode: row.permission_mode.clone(),
        allowed_tools: row.allowed_tools.clone(),
    }
}

/// The session's recorded working directory — where claude computes the
/// project dir that holds this conversation (F3). Recorded worktree first
/// (must still exist: a reaped worktree's project context is gone — an
/// honest error, not a silent wrong-cwd guess), else the rig path.
fn resume_cwd(config: &CampConfig, row: &SessionRow) -> Result<PathBuf> {
    if let Some(wt) = &row.worktree {
        let path = PathBuf::from(wt);
        if !path.is_dir() {
            bail!(
                "session {}'s worktree {} no longer exists (reaped on close); \
                 its conversation cannot be resumed from its project context",
                row.name,
                path.display()
            );
        }
        return Ok(path);
    }
    let rig = row.rig.as_deref().ok_or_else(|| {
        anyhow!(
            "session {:?} has no rig or worktree recorded; cannot choose a resume cwd",
            row.name
        )
    })?;
    Ok(config.rig(rig)?.path.clone())
}

/// F2 parse rule: the envelope is a JSON array; the element with
/// type=="result" carries the reply. is_error==true fails fast with the
/// result text.
fn parse_result_text(stdout: &[u8]) -> Result<String> {
    let envelope: serde_json::Value =
        serde_json::from_slice(stdout).context("resume output is not the F2 JSON envelope")?;
    let result = envelope
        .as_array()
        .and_then(|a| a.iter().rev().find(|e| e["type"] == "result"))
        .ok_or_else(|| anyhow!("resume envelope has no result element (F2)"))?;
    let text = result["result"]
        .as_str()
        .ok_or_else(|| anyhow!("resume result element has no result text (F2)"))?;
    if result["is_error"].as_bool() == Some(true) {
        bail!("resume reported an error: {text}");
    }
    Ok(text.to_owned())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn parse_result_text_extracts_the_result_element() {
        let envelope = r#"[
            {"type":"system","subtype":"init"},
            {"type":"assistant"},
            {"type":"result","is_error":false,"result":"NUDGE-REPLY","session_id":"sid"}
        ]"#;
        assert_eq!(
            parse_result_text(envelope.as_bytes()).unwrap(),
            "NUDGE-REPLY"
        );
    }

    #[test]
    fn parse_result_text_fails_fast_on_error_results_and_junk() {
        let err_env = r#"[{"type":"result","is_error":true,"result":"boom","session_id":"s"}]"#;
        assert!(parse_result_text(err_env.as_bytes()).is_err());
        assert!(parse_result_text(b"[]").is_err());
        assert!(parse_result_text(b"not json").is_err());
    }
}

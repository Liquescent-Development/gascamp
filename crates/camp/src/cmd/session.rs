//! `camp session register` / `camp session end` (Phase 12, Decision D1):
//! the hook-facing session-lifecycle verbs. The camp plugin's SessionStart
//! and SessionEnd hooks are thin wrappers over these. They append the
//! existing `session.woke` / `session.stopped` event types (added by
//! Phase 8/11) — no new vocabulary — via the same
//! `Ledger::append` + `poke_best_effort` pattern as `event_emit.rs`. The
//! ledger already models a hook-registered attended session (its
//! `session.woke` actor is not `campd`, so patrol treats it annotate-only;
//! with no `--bead` it is not even tracked, so it never counts as `red`).
//!
//! `--hook-stdin` parses the Claude Code hook payload (leniently — the
//! harness sends many fields camp ignores) so the shell hooks stay trivial
//! and dependency-free (no `jq`). A hook-registered session's name is
//! derived deterministically as `attended/<session_id>`, so SessionStart
//! (register) and SessionEnd (end) always agree.

use std::io::Read;

use anyhow::{Result, anyhow};
use camp_core::event::{EventInput, EventType};
use camp_core::ledger::Ledger;
use serde::{Deserialize, Serialize};

use crate::campdir::CampDir;

/// The `session.woke` payload (mirrors the fold's `SessionWoke`, which is
/// `deny_unknown_fields`): omit absent optionals so the fold accepts it.
#[derive(Serialize)]
struct WokeData<'a> {
    name: &'a str,
    agent: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    rig: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    claude_session_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    transcript_path: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pid: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bead: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    worktree: Option<&'a str>,
    /// The rig's base commit at registration (Phase 3, Q4): the same
    /// dispatch-time fact campd records, so the shipped gate has a descent
    /// reference for attended sessions too. Absent without `--rig` or when
    /// the rig has no base commit.
    #[serde(skip_serializing_if = "Option::is_none")]
    base: Option<&'a str>,
}

/// The `session.stopped` payload (mirrors the fold's `SessionEnd`).
#[derive(Serialize)]
struct EndData<'a> {
    name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    signal: Option<i64>,
}

/// A Claude Code hook stdin payload. Lenient by design (NO
/// `deny_unknown_fields`): this is the harness's schema, not a camp event,
/// and it carries many fields camp does not use.
#[derive(Deserialize)]
struct HookInput {
    session_id: String,
    #[serde(default)]
    transcript_path: Option<String>,
    /// SessionEnd/SessionStart `source` (e.g. "startup", "prompt_input_exit").
    #[serde(default)]
    source: Option<String>,
}

fn read_hook_stdin() -> Result<HookInput> {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    Ok(serde_json::from_str(&buf)?)
}

/// `attended/<session_id>` — the deterministic registry name for a
/// hook-registered attended session (register and end derive it identically).
fn attended_name(session_id: &str) -> String {
    format!("attended/{session_id}")
}

#[allow(clippy::too_many_arguments)]
pub fn register(
    camp: &CampDir,
    name: Option<String>,
    agent: Option<String>,
    rig: Option<String>,
    session_id: Option<String>,
    transcript: Option<String>,
    pid: Option<i64>,
    bead: Option<String>,
    worktree: Option<String>,
    actor: String,
    hook_stdin: bool,
) -> Result<()> {
    let mut ledger = Ledger::open(&camp.db_path())?;
    let (name, agent, session_id, transcript) = if hook_stdin {
        let input = read_hook_stdin()?;
        (
            attended_name(&input.session_id),
            "attended".to_owned(),
            Some(input.session_id),
            input.transcript_path,
        )
    } else {
        let name = name.ok_or_else(|| anyhow!("--name is required unless --hook-stdin"))?;
        let agent = agent.ok_or_else(|| anyhow!("--agent is required unless --hook-stdin"))?;
        (name, agent, session_id, transcript)
    };

    // Idempotent: session names are fold-unique forever. A repeat
    // SessionStart (resume/clear) — or a resumed session whose row already
    // ended — must not attempt a duplicate session.woke.
    if let Some(status) = ledger.session_status(&name)? {
        eprintln!("camp: session {name:?} already registered (status {status}); skipping");
        return Ok(());
    }

    // Phase 3 (Q4): record the rig's base commit at registration, exactly
    // as campd does at dispatch. An unconfigured --rig name errors through
    // config.rig — fail fast.
    let base = match rig.as_deref() {
        Some(r) => {
            let config = camp_core::config::CampConfig::load(&camp.config_path())?;
            crate::daemon::spawn::rig_base(&config.rig(r)?.path, config.dispatch.exec_timeout()?)?
        }
        None => None,
    };
    let data = serde_json::to_value(WokeData {
        name: &name,
        agent: &agent,
        rig: rig.as_deref(),
        claude_session_id: session_id.as_deref(),
        transcript_path: transcript.as_deref(),
        pid,
        bead: bead.as_deref(),
        worktree: worktree.as_deref(),
        base: base.as_deref(),
    })?;
    let seq = ledger.append(EventInput {
        kind: EventType::SessionWoke,
        rig,
        actor,
        bead,
        data,
    })?;
    crate::daemon::socket::poke_best_effort(camp, seq);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn end(
    camp: &CampDir,
    name: Option<String>,
    reason: Option<String>,
    exit_code: Option<i64>,
    signal: Option<i64>,
    actor: String,
    hook_stdin: bool,
    if_registered: bool,
) -> Result<()> {
    let mut ledger = Ledger::open(&camp.db_path())?;
    let (name, reason) = if hook_stdin {
        let input = read_hook_stdin()?;
        (attended_name(&input.session_id), input.source.or(reason))
    } else {
        let name = name.ok_or_else(|| anyhow!("--name is required unless --hook-stdin"))?;
        (name, reason)
    };

    // --if-registered: only a currently-live session can be ended; anything
    // else (unregistered, or already ended) is a clean no-op. Without the
    // flag, an unknown/non-live session is a hard error (fold enforces it).
    if if_registered && ledger.session_status(&name)?.as_deref() != Some("live") {
        return Ok(());
    }

    let data = serde_json::to_value(EndData {
        name: &name,
        reason: reason.as_deref(),
        exit_code,
        signal,
    })?;
    let seq = ledger.append(EventInput {
        kind: EventType::SessionStopped,
        rig: None,
        actor,
        bead: None,
        data,
    })?;
    crate::daemon::socket::poke_best_effort(camp, seq);
    Ok(())
}

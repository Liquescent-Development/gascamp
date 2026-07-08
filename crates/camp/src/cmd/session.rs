//! `camp session register` / `camp session end` (Phase 12, Decision D1):
//! the hook-facing session-lifecycle verbs. The camp plugin's SessionStart
//! and SessionEnd hooks are thin wrappers over these. They append the
//! existing `session.woke` / `session.stopped` event types (added by
//! Phase 8/11) — no new vocabulary — via the same
//! `Ledger::append` + `poke_best_effort` pattern as `event_emit.rs`. The
//! ledger already models a hook-registered attended session (its
//! `session.woke` actor is not `campd`, so patrol treats it annotate-only).

use anyhow::Result;
use camp_core::event::{EventInput, EventType};
use camp_core::ledger::Ledger;
use serde::Serialize;

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

/// Flags for `camp session register`. `actor` defaults to
/// `hook:session-start` — the provenance the ledger uses to mark a
/// hook-registered (annotate-only) session.
#[allow(clippy::too_many_arguments)]
pub fn register(
    camp: &CampDir,
    name: String,
    agent: String,
    rig: Option<String>,
    session_id: Option<String>,
    transcript: Option<String>,
    pid: Option<i64>,
    bead: Option<String>,
    worktree: Option<String>,
    actor: String,
) -> Result<()> {
    let mut ledger = Ledger::open(&camp.db_path())?;
    let data = serde_json::to_value(WokeData {
        name: &name,
        agent: &agent,
        rig: rig.as_deref(),
        claude_session_id: session_id.as_deref(),
        transcript_path: transcript.as_deref(),
        pid,
        bead: bead.as_deref(),
        worktree: worktree.as_deref(),
    })?;
    let seq = ledger.append(EventInput {
        kind: EventType::SessionWoke,
        rig,
        actor,
        bead,
        data,
    })?;
    crate::daemon::socket::poke_best_effort(&camp.socket_path(), seq);
    Ok(())
}

/// Flags for `camp session end`. `actor` defaults to `hook:session-end`.
pub fn end(
    camp: &CampDir,
    name: String,
    reason: Option<String>,
    exit_code: Option<i64>,
    signal: Option<i64>,
    actor: String,
) -> Result<()> {
    let mut ledger = Ledger::open(&camp.db_path())?;
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
    crate::daemon::socket::poke_best_effort(&camp.socket_path(), seq);
    Ok(())
}

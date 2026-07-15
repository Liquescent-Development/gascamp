//! compat §6.1 — the ONE projection of a claimed bead row.
//!
//! The claim invariant is: one bead row, three byte-projections (the hook JSON,
//! `bd show --json`, and the worker env). This module is the read side shared by
//! the hook and `bd show`. It does NOT re-hand-roll the "assignee column IS
//! `gc.routed_to`" mapping — that stays owned by `readiness::PROJECTED_METADATA`
//! / `readiness::bead_metadata` (the one formatter). `assignee` here is the
//! SESSION (`claimed_by`), read directly; `route`/`work_branch` come back
//! THROUGH `bead_metadata` (B5/B6).

use anyhow::{Result, anyhow};
use camp_core::ledger::Ledger;

/// A claimed bead row, projected. The field names follow gc's orientation:
/// `assignee` is the SESSION (camp's `claimed_by`), `route` is the qualified
/// agent (camp's `assignee` column, projected as `gc.routed_to`).
pub struct ClaimProjection {
    /// gc's `assignee` = camp's `claimed_by` (the worker session).
    pub assignee: Option<String>,
    /// gc's `gc.routed_to` = camp's `assignee` column (the cooked route) — read
    /// back through `bead_metadata`, NOT re-derived here or from env.
    pub route: Option<String>,
    /// gc's `gc.work_branch` = camp's `work_branch` column (the dispatch
    /// branch) — also through `bead_metadata`.
    pub work_branch: Option<String>,
}

/// Project bead `bead`'s claim fields. `assignee = claimed_by` (a direct read of
/// the session column); `route`/`work_branch` come from `readiness::bead_metadata`
/// (the single formatter that maps the dedicated columns to `gc.routed_to` /
/// `gc.work_branch`). Returns `Err` only if the bead does not exist.
pub fn claim_projection(ledger: &Ledger, bead: &str) -> Result<ClaimProjection> {
    let row = ledger
        .bead_row(bead)?
        .ok_or_else(|| anyhow!("no such bead {bead}"))?;
    let meta = ledger.bead_metadata(bead)?;
    Ok(ClaimProjection {
        assignee: row.claimed_by,
        route: meta.get("gc.routed_to").cloned(),
        work_branch: meta.get("gc.work_branch").cloned(),
    })
}

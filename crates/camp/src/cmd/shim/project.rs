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

/// The hook's view of a claimed bead row. `assignee` is the SESSION (camp's
/// `claimed_by`); `route` is the qualified agent (camp's `assignee` column,
/// projected as `gc.routed_to`). The dispatch branch (`gc.work_branch`) is NOT
/// carried here — the hook does not print it; `bd show --json` projects it
/// straight from `readiness::bead_metadata` (B5), so it never needs a second
/// home in shim code.
pub struct ClaimProjection {
    /// gc's `assignee` = camp's `claimed_by` (the worker session).
    pub assignee: Option<String>,
    /// gc's `gc.routed_to` = camp's `assignee` column (the cooked route) — read
    /// back through `bead_metadata`, NOT re-derived here or from env.
    pub route: Option<String>,
}

/// Project bead `bead`'s claim fields for the hook. `assignee = claimed_by` (a
/// direct read of the session column); `route` comes from
/// `readiness::bead_metadata` (the single formatter that maps the `assignee`
/// column to `gc.routed_to`). Returns `Err` only if the bead does not exist.
pub fn claim_projection(ledger: &Ledger, bead: &str) -> Result<ClaimProjection> {
    let row = ledger
        .bead_row(bead)?
        .ok_or_else(|| anyhow!("no such bead {bead}"))?;
    let meta = ledger.bead_metadata(bead)?;
    Ok(ClaimProjection {
        assignee: row.claimed_by,
        route: meta.get("gc.routed_to").cloned(),
    })
}

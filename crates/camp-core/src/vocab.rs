//! The spec §15.2 vocabulary mirror. Every event name camp emits is declared
//! here as either gc-mirrored (spelling matches Gas City verbatim) or
//! camp-specific (additive — must NOT exist in gc's registry). Tests in
//! tests/vocab_pin.rs enforce both directions against the pinned gc
//! reference (tests/fixtures/gc-vocab.json), and the Phase 6 CI job
//! re-verifies the pin against gascity source at ci/gc-compat/GASCITY_REF.

/// Names camp shares with Gas City — spelling matches gc verbatim.
pub const GC_MIRRORED_EVENTS: &[&str] = &[
    "bead.created",
    "bead.updated",
    "bead.closed",
    "session.woke",
    "session.stopped",
    "session.crashed",
    "order.fired",
    "order.completed",
    "order.failed",
    "bead.worktree.reaped",
];

/// Camp-specific names — additive; never redefinitions of gc names.
pub const CAMP_SPECIFIC_EVENTS: &[&str] = &[
    "bead.claimed",
    "campd.started",
    "campd.stopped",
    "campd.autostarted",
    "config.changed",
    "rig.added",
    "run.cooked",
    "worker.milestone",
    "worktree.kept",
    "dispatch.failed",
    "dispatch.live_tree",
    "check.passed",
    "check.failed",
    "run.finalized",
    "agent.stalled",
    "patrol.degraded",
];

/// Values `bead.closed` accepts for `outcome` — a strict subset of gc's
/// outcome vocabulary (spec §8.2). `skipped` is campd's finalization close
/// for steps whose `needs` can never be satisfied (Phase 9 plan Decision 2
/// — gc's own word for exactly this).
pub const CAMP_OUTCOMES: &[&str] = &["pass", "fail", "skipped"];

/// Values `bead.closed` accepts for `final_disposition` (retry exhaustion,
/// Phase 9) — a strict subset of gc's, and exactly gc's legal
/// `on_exhausted` values. A close NEVER carries "pass": the run-level pass
/// disposition lives only in `run.finalized` (CAMP_RUN_DISPOSITIONS).
pub const CAMP_FINAL_DISPOSITIONS: &[&str] = &["hard_fail", "soft_fail"];

/// Values `run.finalized` accepts for its run-level `final_disposition` — a
/// strict subset of gc's `final_disposition` vocabulary (Phase 9 plan
/// Decision 3 as revised per review Blocker A).
pub const CAMP_RUN_DISPOSITIONS: &[&str] = &["pass", "hard_fail", "soft_fail"];

/// Values `bead.closed` accepts for `failure_class` (spec §8.2 retry
/// classification: `camp close --outcome fail --transient`). The gc-vocab
/// fixture carries no failure_class list; this follows the master plan's
/// wording (`failure_class:"transient"`, gc's key vocabulary) verbatim.
pub const CAMP_FAILURE_CLASSES: &[&str] = &["transient"];

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// Test obligation (iv), dispatch-lifecycle Phase 1 (#29): the
    /// deprecated reservation design leaked no vocabulary.
    #[test]
    fn no_reservation_vocabulary_exists() {
        for name in GC_MIRRORED_EVENTS.iter().chain(CAMP_SPECIFIC_EVENTS) {
            assert!(
                !name.contains("reserv") && !name.contains("attended"),
                "reservation-era name leaked into the vocabulary: {name}"
            );
        }
    }
}

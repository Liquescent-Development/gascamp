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
];

/// Camp-specific names — additive; never redefinitions of gc names.
pub const CAMP_SPECIFIC_EVENTS: &[&str] = &[
    "bead.claimed",
    "campd.started",
    "campd.stopped",
    "campd.autostarted",
    "rig.added",
    "run.cooked",
];

/// Values `bead.closed` accepts for `outcome` — a strict subset of gc's
/// outcome vocabulary (spec §8.2).
pub const CAMP_OUTCOMES: &[&str] = &["pass", "fail"];

/// Values camp uses for `final_disposition` (retry exhaustion, Phase 9) — a
/// strict subset of gc's, and exactly gc's legal `on_exhausted` values.
pub const CAMP_FINAL_DISPOSITIONS: &[&str] = &["hard_fail", "soft_fail"];

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
    "campd.autostarted", // historical: no producer since the CLI became a pure client
    "config.changed",
    "rig.added",
    "run.cooked",
    "worker.milestone",
    "worktree.kept",
    "dispatch.failed",
    "dispatch.live_tree",
    "dispatch.rearmed",
    "check.passed",
    "check.failed",
    "run.finalized",
    "agent.stalled",
    "patrol.degraded",
    "session.stream_capped",
    "session.nudged",
    // cp-1 (control-plane spec §2.1/§4.4): the control plane. None of these
    // four names exists in gc's registry, so all four are additive.
    "session.interrupted",
    "control.responded",
    "control.failed",
    "subscriber.dropped",
    "import.added",
    "import.refused",
];

/// Values `bead.closed` accepts for `outcome` — a strict subset of gc's
/// outcome vocabulary (spec §8.2). `skipped` is campd's finalization close
/// for steps whose `needs` can never be satisfied (Phase 9 plan Decision 2
/// — gc's own word for exactly this).
pub const CAMP_OUTCOMES: &[&str] = &["pass", "fail", "skipped"];

/// Values `bead.closed` accepts for `work_outcome` — Gas City's WorkOutcome
/// axis (`gc.work_outcome`, ADR-0009 at the pinned ref), mirrored VERBATIM
/// as a SEPARATE, additive axis from the control `outcome` (dispatch-
/// lifecycle Q3, REVISED & SETTLED 2026-07-09). Un-integrable work is
/// `blocked` here, never a new control-outcome value. Only `shipped`
/// carries an artifact (a commit on the work branch); the "shipped requires
/// a reachable, based commit" rule is owned by the `camp close` gate, not
/// declared here — exactly gc's division (values.go vs cmd/gc).
pub const CAMP_WORK_OUTCOMES: &[&str] = &["shipped", "no-op", "blocked", "abandoned"];

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

/// Why a control request failed (§2.1). **A CLOSED SET, and the partition into
/// TERMINAL vs CORRECTABLE is a COMPILE-TIME obligation.**
///
/// It used to be two independent hand-maintained string arrays — one in the fold
/// (for validation), one in the daemon (for rehydration routing). They had to
/// partition consistently, and nothing made them. Adding a ninth cause to the fold
/// without classifying it in the daemon made campd `bail!` at STARTUP on any ledger
/// containing it: loud (invariant 5), but a footgun for the phases that will add
/// causes. Here, a new variant that is not classified below **does not compile**.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlFailureCause {
    /// The session went quiet with the request unanswered.
    SilenceTimeout,
    /// The session kept producing output but never answered within the ceiling.
    CeilingTimeout,
    /// The session was disposed with the request still unanswered.
    SessionEnded,
    /// The pipe write itself failed; the request never reached the worker.
    WriteFailed,
    /// A `control_response` for an id camp never sent.
    UnknownRequest,
    /// A control message camp could not parse.
    Unparsable,
    /// A `request_user_dialog` met camp's deterministic refusal.
    DialogRefused,
    /// A `can_use_tool` arrived, which cp-1 cannot answer (§5.3.1).
    PermissionUnanswerable,
}

impl ControlFailureCause {
    pub const ALL: &'static [ControlFailureCause] = &[
        ControlFailureCause::SilenceTimeout,
        ControlFailureCause::CeilingTimeout,
        ControlFailureCause::SessionEnded,
        ControlFailureCause::WriteFailed,
        ControlFailureCause::UnknownRequest,
        ControlFailureCause::Unparsable,
        ControlFailureCause::DialogRefused,
        ControlFailureCause::PermissionUnanswerable,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ControlFailureCause::SilenceTimeout => "silence_timeout",
            ControlFailureCause::CeilingTimeout => "ceiling_timeout",
            ControlFailureCause::SessionEnded => "session_ended",
            ControlFailureCause::WriteFailed => "write_failed",
            ControlFailureCause::UnknownRequest => "unknown_request",
            ControlFailureCause::Unparsable => "unparsable",
            ControlFailureCause::DialogRefused => "dialog_refused",
            ControlFailureCause::PermissionUnanswerable => "permission_unanswerable",
        }
    }

    pub fn parse(s: &str) -> Option<ControlFailureCause> {
        ControlFailureCause::ALL
            .iter()
            .find(|c| c.as_str() == s)
            .copied()
    }

    /// TERMINAL: no answer can EVER arrive, so a `control_response` for a request
    /// settled by this cause is a DUPLICATE, never a correction.
    ///
    /// The two false cases are the whole reason this distinction exists: campd said
    /// "no answer came", and an answer may yet come — so a late `control_response`
    /// must append `control.responded{late:true}` and CORRECT the premature fault,
    /// not be swallowed. **The `match` is exhaustive: a new variant must decide.**
    pub fn is_terminal(self) -> bool {
        match self {
            // Correctable — the request may still be answered.
            ControlFailureCause::SilenceTimeout | ControlFailureCause::CeilingTimeout => false,
            // Terminal — nothing can ever arrive for these.
            ControlFailureCause::SessionEnded
            | ControlFailureCause::WriteFailed
            | ControlFailureCause::UnknownRequest
            | ControlFailureCause::Unparsable
            | ControlFailureCause::DialogRefused
            | ControlFailureCause::PermissionUnanswerable => true,
        }
    }
}

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
    "formula.refused",
    // compat §6 (worker contract). Both additive; neither is a gc name nor a
    // prefix-truncation of one (B4):
    //   `shim.refused` — a shim was handed a verb/flag camp does not serve.
    //     Follows the merged `import.refused`/`formula.refused` precedent; gc
    //     has no `shim.*` name.
    //   `worker.drain_acked` — camp's release trigger. gc's registry carries
    //     `session.drain_acked_with_assigned_work`, `session.draining`,
    //     `session.undrained`, but camp's is a DISTINCT concept: camp truncates
    //     gc's continuation loop (§6.2), so a worker is one-bead-per-session and
    //     has NO assigned work remaining at drain-ack — this is campd's internal
    //     RELEASE TRIGGER, not gc's session-still-holding-work STATE. The
    //     `worker.*` namespace is camp's (`worker.milestone`); gc's only
    //     `worker.*` name is `worker.operation`, so `worker.drain_acked` is
    //     neither a gc name nor a prefix-truncation of one.
    "shim.refused",
    "worker.drain_acked",
    // cp-3 (control-plane §5.3): the permission plane. gc has no `permission.*`
    // event, so all three are additive, never redefinitions.
    "permission.pending",
    "permission.decided",
    "permission.saturated",
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
    /// Every cause, in ordinal order.
    ///
    /// **WHAT IS COMPILE-ENFORCED, EXACTLY — and this comment claims nothing more:**
    /// - the **PARTITION** (terminal vs correctable): `is_terminal` and `as_str` are
    ///   exhaustive matches, so a new variant CANNOT be added without classifying it.
    ///   That is the guarantee that matters, and it holds.
    /// - `ALL`'s **internal consistency**: the `const` block below proves every entry
    ///   sits at its own `ordinal()`, so `ALL` cannot be reordered, duplicated, or
    ///   have an entry swapped under an index.
    ///
    /// **WHAT IS *NOT* COMPILE-ENFORCED: MEMBERSHIP.** A new variant classified in
    /// both matches but never added to `ALL` still compiles. Rust cannot enumerate
    /// variants at compile time without `strum` or the unstable `variant_count`, and
    /// this crate takes no new dependencies — so this is a real, named limit, not an
    /// oversight.
    ///
    /// **The consequence of forgetting is LOUD, not silent** (invariant 5): the fold
    /// refuses an event whose cause will not `parse`, and `rehydrate` `bail!`s rather
    /// than guess. You find out at the first append or the next campd start — not
    /// three phases later, and never by silent misrouting. That is why this is
    /// acceptable, and it is why the failure mode is written down here.
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

    /// Each variant's index in `ALL`. EXHAUSTIVE on purpose: a new variant does not
    /// compile until it is given an ordinal, and the assertion below then forces
    /// `ALL` to actually contain it at that index.
    const fn ordinal(self) -> usize {
        match self {
            ControlFailureCause::SilenceTimeout => 0,
            ControlFailureCause::CeilingTimeout => 1,
            ControlFailureCause::SessionEnded => 2,
            ControlFailureCause::WriteFailed => 3,
            ControlFailureCause::UnknownRequest => 4,
            ControlFailureCause::Unparsable => 5,
            ControlFailureCause::DialogRefused => 6,
            ControlFailureCause::PermissionUnanswerable => 7,
        }
    }

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

// ---------------------------------------------------------------------------
// `ControlFailureCause::ALL` is INTERNALLY CONSISTENT, at compile time.
//
// Every entry sits at its own `ordinal()`, so ALL cannot be reordered, duplicated,
// or have an entry swapped out from under an index.
//
// **THIS DOES NOT PROVE MEMBERSHIP** — a variant classified in `as_str`/`is_terminal`
// but never added to ALL still compiles, because Rust cannot enumerate variants at
// compile time without `strum` or the unstable `variant_count`, and this crate takes
// no new dependencies. That limit is stated on `ALL` itself rather than papered over:
// forgetting is LOUD (the fold refuses the cause; `rehydrate` bails), never silent.
const _: () = {
    let mut i = 0;
    while i < ControlFailureCause::ALL.len() {
        assert!(
            ControlFailureCause::ALL[i].ordinal() == i,
            "ControlFailureCause::ALL is out of ordinal order"
        );
        i += 1;
    }
};

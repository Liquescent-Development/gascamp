//! Health patrol state machines (spec §10): pure, deterministic, no I/O.
//! Durations are jiff friendly strings ("10m"); the pure machines take
//! explicit `now: Timestamp` (the CronHeap precedent). Patrol config is
//! read at campd start; hot reload does not re-arm patrol (Phase 11 plan
//! Decision L).

pub mod timers;

use std::collections::HashMap;

use jiff::SignedDuration;

use crate::config::PatrolSection;
use crate::error::CoreError;

/// Parse a strictly positive friendly duration ("10m", "90s", "300ms").
pub fn parse_duration(s: &str) -> Result<SignedDuration, CoreError> {
    let d: SignedDuration = s
        .parse()
        .map_err(|e| CoreError::Config(format!("[patrol] duration {s:?} does not parse: {e}")))?;
    if d.is_negative() || d.is_zero() {
        return Err(CoreError::Config(format!(
            "[patrol] duration {s:?} must be strictly positive"
        )));
    }
    Ok(d)
}

/// `[patrol]` resolved to typed values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatrolConfig {
    pub stall_after: SignedDuration,
    pub restart_budget: u32,
    pub release_grace: SignedDuration,
}

impl PatrolConfig {
    pub fn from_section(section: &PatrolSection) -> Result<PatrolConfig, CoreError> {
        Ok(PatrolConfig {
            stall_after: parse_duration(&section.stall_after)?,
            restart_budget: section.restart_budget,
            release_grace: parse_duration(&section.release_grace)?,
        })
    }
}

/// What the ladder tells patrol to do about a stall fire (spec §10.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LadderAction {
    /// Deliver a status-request turn (stdin for held workers, resume
    /// otherwise).
    Nudge,
    /// Kill and respawn; the bead re-hooks through normal dispatch.
    Restart,
    /// Budget spent: emit and stop — escalation to judgment is pack
    /// content (an order matching event:agent.stalled), never Rust.
    Exhausted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Next {
    Nudge,
    Restart,
}

#[derive(Debug, Clone, Copy)]
struct LadderState {
    restarts: u32,
    next: Next,
}

/// The mechanical escalation ladder, per BEAD (the bead is the work;
/// sessions are disposable). Fire sequence per worker generation is
/// nudge → (still silent) → restart; `restart_budget` bounds restarts per
/// bead per campd lifetime (in-memory, crash-only — the Dispatcher::failed
/// precedent); activity rewinds the next step to nudge but keeps the
/// restart count. Known and accepted: a worker that answers every nudge
/// but never closes its bead oscillates nudge→activity→nudge — mechanical
/// patrol cannot judge progress; the repeated agent.stalled trail is the
/// escalation surface.
#[derive(Debug)]
pub struct Ladder {
    restart_budget: u32,
    states: HashMap<String, LadderState>,
}

impl Ladder {
    pub fn new(restart_budget: u32) -> Ladder {
        Ladder {
            restart_budget,
            states: HashMap::new(),
        }
    }

    /// A stall timer fired for this bead's worker: what happens now.
    pub fn on_fire(&mut self, bead: &str) -> LadderAction {
        let state = self.states.entry(bead.to_owned()).or_insert(LadderState {
            restarts: 0,
            next: Next::Nudge,
        });
        match state.next {
            Next::Nudge => {
                state.next = Next::Restart;
                LadderAction::Nudge
            }
            Next::Restart => {
                if state.restarts < self.restart_budget {
                    state.restarts += 1;
                    state.next = Next::Nudge;
                    LadderAction::Restart
                } else {
                    LadderAction::Exhausted
                }
            }
        }
    }

    /// Activity observed: the next stall starts at nudge again. The
    /// restart count persists — revival does not refund the budget.
    pub fn on_activity(&mut self, bead: &str) {
        if let Some(state) = self.states.get_mut(bead) {
            state.next = Next::Nudge;
        }
    }

    /// A nudge could not be DELIVERED: skip straight to restart next fire.
    /// Creates the state when absent — a failed nudge implies a nudge was
    /// attempted, so the bead is on the ladder by definition.
    pub fn nudge_failed(&mut self, bead: &str) {
        self.states
            .entry(bead.to_owned())
            .or_insert(LadderState {
                restarts: 0,
                next: Next::Nudge,
            })
            .next = Next::Restart;
    }

    pub fn restarts(&self, bead: &str) -> u32 {
        self.states.get(bead).map_or(0, |s| s.restarts)
    }

    /// Exponential backoff as threshold scaling (plan Decision D):
    /// base × 2^restarts, saturating — successive restarts space out
    /// exponentially with zero hidden dispatch state.
    pub fn effective_threshold(&self, bead: &str, base: SignedDuration) -> SignedDuration {
        let restarts = self.restarts(bead).min(20); // 2^20 ≫ any real budget
        base.checked_mul(1_i32 << restarts)
            .unwrap_or(SignedDuration::MAX)
    }

    /// The bead closed: its ladder history is over.
    pub fn forget(&mut self, bead: &str) {
        self.states.remove(bead);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use jiff::SignedDuration;

    #[test]
    fn parse_duration_accepts_friendly_forms() {
        assert_eq!(
            parse_duration("10m").unwrap(),
            SignedDuration::from_mins(10)
        );
        assert_eq!(
            parse_duration("90s").unwrap(),
            SignedDuration::from_secs(90)
        );
        assert_eq!(
            parse_duration("300ms").unwrap(),
            SignedDuration::from_millis(300)
        );
    }

    #[test]
    fn parse_duration_rejects_zero_negative_and_junk() {
        for bad in ["0s", "-5m", "", "banana", "10"] {
            let err = parse_duration(bad).unwrap_err();
            assert!(
                err.to_string().contains("patrol"),
                "{bad:?}: error must locate the [patrol] duration: {err}"
            );
        }
    }

    #[test]
    fn patrol_config_resolves_a_section() {
        let section = crate::config::PatrolSection::default();
        let cfg = PatrolConfig::from_section(&section).unwrap();
        assert_eq!(cfg.stall_after, SignedDuration::from_mins(10));
        assert_eq!(cfg.restart_budget, 2);
        assert_eq!(cfg.release_grace, SignedDuration::from_secs(30));
    }

    // ---- the ladder (master plan: nudge -> restart -> exhausted) ---------

    #[test]
    fn ladder_table_nudge_restart_exhausted_with_budget_two() {
        let mut l = Ladder::new(2);
        // generation 1: nudge, then restart 1
        assert_eq!(l.on_fire("gc-1"), LadderAction::Nudge);
        assert_eq!(l.on_fire("gc-1"), LadderAction::Restart);
        assert_eq!(l.restarts("gc-1"), 1);
        // generation 2 (respawned worker): nudge again, then restart 2
        assert_eq!(l.on_fire("gc-1"), LadderAction::Nudge);
        assert_eq!(l.on_fire("gc-1"), LadderAction::Restart);
        assert_eq!(l.restarts("gc-1"), 2);
        // budget exhausted: the next needed restart emits-and-stops
        assert_eq!(l.on_fire("gc-1"), LadderAction::Nudge);
        assert_eq!(l.on_fire("gc-1"), LadderAction::Exhausted);
        assert_eq!(
            l.on_fire("gc-1"),
            LadderAction::Exhausted,
            "exhausted is terminal"
        );
        assert_eq!(l.restarts("gc-1"), 2, "exhaustion does not consume budget");
    }

    #[test]
    fn budget_zero_never_restarts() {
        let mut l = Ladder::new(0);
        assert_eq!(l.on_fire("gc-1"), LadderAction::Nudge);
        assert_eq!(l.on_fire("gc-1"), LadderAction::Exhausted);
    }

    #[test]
    fn activity_rewinds_to_nudge_but_keeps_the_restart_count() {
        let mut l = Ladder::new(2);
        assert_eq!(l.on_fire("gc-1"), LadderAction::Nudge);
        l.on_activity("gc-1"); // the nudge revived it
        assert_eq!(
            l.on_fire("gc-1"),
            LadderAction::Nudge,
            "a revived worker is nudged first again"
        );
        assert_eq!(l.on_fire("gc-1"), LadderAction::Restart);
        l.on_activity("gc-1");
        assert_eq!(l.restarts("gc-1"), 1, "revival does not refund the budget");
    }

    #[test]
    fn a_failed_nudge_advances_to_restart() {
        let mut l = Ladder::new(2);
        assert_eq!(l.on_fire("gc-1"), LadderAction::Nudge);
        l.nudge_failed("gc-1");
        assert_eq!(l.on_fire("gc-1"), LadderAction::Restart);
    }

    #[test]
    fn backoff_series_doubles_the_threshold_per_restart() {
        let mut l = Ladder::new(3);
        let base = SignedDuration::from_mins(10);
        assert_eq!(
            l.effective_threshold("gc-1", base),
            SignedDuration::from_mins(10)
        );
        l.on_fire("gc-1");
        l.on_fire("gc-1"); // nudge, restart 1
        assert_eq!(
            l.effective_threshold("gc-1", base),
            SignedDuration::from_mins(20)
        );
        l.on_fire("gc-1");
        l.on_fire("gc-1"); // nudge, restart 2
        assert_eq!(
            l.effective_threshold("gc-1", base),
            SignedDuration::from_mins(40)
        );
        l.on_fire("gc-1");
        l.on_fire("gc-1"); // nudge, restart 3
        assert_eq!(
            l.effective_threshold("gc-1", base),
            SignedDuration::from_mins(80)
        );
    }

    #[test]
    fn forget_clears_bead_state() {
        let mut l = Ladder::new(1);
        l.on_fire("gc-1");
        l.on_fire("gc-1");
        l.forget("gc-1");
        assert_eq!(l.restarts("gc-1"), 0);
        assert_eq!(l.on_fire("gc-1"), LadderAction::Nudge);
    }
}

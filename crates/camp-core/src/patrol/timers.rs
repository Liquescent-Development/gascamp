//! The patrol timer store (spec §10): one armed timer per tracked session,
//! deadline-sourced poll timeout — the same mechanism as the cron heap,
//! never a tick. A small map with min-scan (Phase 11 plan Decision B):
//! active workers are bounded by [dispatch] max_workers, so O(n) scans are
//! honest and reset/disarm stay O(1) by session key.

use std::collections::HashMap;
use std::time::Duration;

use jiff::{SignedDuration, Timestamp};

/// What a deadline means when it fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimerKind {
    /// Silence threshold: fire = the worker looks stalled (spec §10.2).
    Stall,
    /// Release grace: fire = a released stream worker is still alive and
    /// gets terminated (Phase 11 plan Decision C2, probe P3).
    Release,
}

/// A due deadline popped by `fire_due`.
#[derive(Debug, Clone, PartialEq)]
pub struct StallFire {
    pub session: String,
    pub kind: TimerKind,
    pub deadline: Timestamp,
    pub threshold: SignedDuration,
}

/// now + threshold, clamped to the timestamp range (no panic in lib code;
/// a MAX deadline simply never fires — thresholds are validated positive).
fn deadline_of(now: Timestamp, threshold: SignedDuration) -> Timestamp {
    now.checked_add(threshold).unwrap_or(Timestamp::MAX)
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    kind: TimerKind,
    deadline: Timestamp,
    threshold: SignedDuration,
}

/// One armed timer per tracked session (upsert by session name).
#[derive(Debug, Default)]
pub struct StallTimers {
    entries: HashMap<String, Entry>,
}

impl StallTimers {
    pub fn new() -> StallTimers {
        StallTimers::default()
    }

    /// Arm (or re-arm) a session's timer: deadline = now + threshold.
    pub fn arm(&mut self, session: &str, kind: TimerKind, threshold: SignedDuration, now: Timestamp) {
        self.entries.insert(
            session.to_owned(),
            Entry {
                kind,
                deadline: deadline_of(now, threshold),
                threshold,
            },
        );
    }

    /// Activity observed: push a Stall deadline to now + threshold.
    /// Release grace deliberately ignores activity (the worker's work is
    /// done; only its exit matters). Returns whether a timer moved.
    pub fn reset(&mut self, session: &str, now: Timestamp) -> bool {
        match self.entries.get_mut(session) {
            Some(entry) if entry.kind == TimerKind::Stall => {
                entry.deadline = deadline_of(now, entry.threshold);
                true
            }
            _ => false,
        }
    }

    pub fn disarm(&mut self, session: &str) -> bool {
        self.entries.remove(session).is_some()
    }

    pub fn is_armed(&self, session: &str) -> bool {
        self.entries.contains_key(session)
    }

    pub fn next_deadline(&self) -> Option<Timestamp> {
        self.entries.values().map(|e| e.deadline).min()
    }

    /// Pop every due deadline, ordered by (deadline, session) so firing is
    /// deterministic. Fired sessions are removed until the caller re-arms.
    pub fn fire_due(&mut self, now: Timestamp) -> Vec<StallFire> {
        let mut due: Vec<String> = self
            .entries
            .iter()
            .filter(|(_, e)| e.deadline <= now)
            .map(|(s, _)| s.clone())
            .collect();
        due.sort_by(|a, b| {
            let (ea, eb) = (&self.entries[a], &self.entries[b]);
            ea.deadline.cmp(&eb.deadline).then_with(|| a.cmp(b))
        });
        due.into_iter()
            .filter_map(|session| {
                self.entries.remove(&session).map(|e| StallFire {
                    session,
                    kind: e.kind,
                    deadline: e.deadline,
                    threshold: e.threshold,
                })
            })
            .collect()
    }

    /// The earliest deadline as a poll timeout — the exact orders shape
    /// (spec §9, invariant 1): `None` = no timers = infinite wait; due or
    /// past = ZERO; otherwise the remaining time rounded UP 1 ms so the
    /// wake lands at-or-after the deadline, never a hot spin just before.
    pub fn poll_timeout(&self, now: Timestamp) -> Option<Duration> {
        let deadline = self.next_deadline()?;
        let until = deadline.duration_since(now);
        if until.is_negative() || until.is_zero() {
            return Some(Duration::ZERO);
        }
        let until = Duration::try_from(until).unwrap_or(Duration::MAX);
        Some(until.saturating_add(Duration::from_millis(1)))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use jiff::{SignedDuration, Timestamp};

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    #[test]
    fn arm_then_threshold_elapses_fires_once_and_disarms() {
        let mut t = StallTimers::new();
        t.arm(
            "c/dev/1",
            TimerKind::Stall,
            SignedDuration::from_mins(10),
            ts("2026-07-07T07:00:00Z"),
        );
        assert!(t.fire_due(ts("2026-07-07T07:09:59Z")).is_empty());
        let fires = t.fire_due(ts("2026-07-07T07:10:00Z"));
        assert_eq!(fires.len(), 1);
        assert_eq!(fires[0].session, "c/dev/1");
        assert_eq!(fires[0].kind, TimerKind::Stall);
        assert_eq!(fires[0].threshold, SignedDuration::from_mins(10));
        assert!(
            !t.is_armed("c/dev/1"),
            "a fired timer is removed until re-armed"
        );
        assert!(t.fire_due(ts("2026-07-07T09:00:00Z")).is_empty());
    }

    #[test]
    fn reset_pushes_the_deadline_out_by_the_threshold() {
        let mut t = StallTimers::new();
        t.arm(
            "s",
            TimerKind::Stall,
            SignedDuration::from_mins(10),
            ts("2026-07-07T07:00:00Z"),
        );
        // transcript touch at 07:09: the deadline moves to 07:19
        assert!(t.reset("s", ts("2026-07-07T07:09:00Z")));
        assert!(
            t.fire_due(ts("2026-07-07T07:10:00Z")).is_empty(),
            "old deadline gone"
        );
        assert_eq!(t.fire_due(ts("2026-07-07T07:19:00Z")).len(), 1);
        assert!(
            !t.reset("ghost", ts("2026-07-07T07:19:00Z")),
            "untracked resets report false"
        );
    }

    #[test]
    fn release_timers_ignore_resets() {
        let mut t = StallTimers::new();
        t.arm(
            "s",
            TimerKind::Release,
            SignedDuration::from_secs(30),
            ts("2026-07-07T07:00:00Z"),
        );
        assert!(
            !t.reset("s", ts("2026-07-07T07:00:10Z")),
            "release grace is not activity-resettable"
        );
        assert_eq!(t.fire_due(ts("2026-07-07T07:00:30Z")).len(), 1);
    }

    #[test]
    fn poll_timeout_mirrors_the_orders_shape() {
        let mut t = StallTimers::new();
        assert_eq!(
            t.poll_timeout(ts("2026-07-07T07:00:00Z")),
            None,
            "idle = infinite wait"
        );
        t.arm(
            "s",
            TimerKind::Stall,
            SignedDuration::from_secs(60),
            ts("2026-07-07T07:00:00Z"),
        );
        let to = t.poll_timeout(ts("2026-07-07T07:00:59Z")).unwrap();
        assert!(
            to >= std::time::Duration::from_secs(1) && to <= std::time::Duration::from_millis(1500),
            "{to:?}"
        );
        assert_eq!(
            t.poll_timeout(ts("2026-07-07T07:02:00Z")),
            Some(std::time::Duration::ZERO),
            "past-due = zero, poll returns immediately and fire_due fires"
        );
    }

    #[test]
    fn fire_due_is_deterministically_ordered_and_disarm_works() {
        let mut t = StallTimers::new();
        for name in ["b", "a", "gone"] {
            t.arm(
                name,
                TimerKind::Stall,
                SignedDuration::from_secs(10),
                ts("2026-07-07T07:00:00Z"),
            );
        }
        assert!(t.disarm("gone"));
        assert!(!t.disarm("gone"), "double disarm reports false");
        let names: Vec<String> = t
            .fire_due(ts("2026-07-07T07:00:10Z"))
            .into_iter()
            .map(|f| f.session)
            .collect();
        assert_eq!(names, vec!["a", "b"], "equal deadlines order by session");
        assert!(t.is_empty());
    }

    #[test]
    fn rearm_overwrites_kind_and_threshold() {
        let mut t = StallTimers::new();
        t.arm(
            "s",
            TimerKind::Stall,
            SignedDuration::from_mins(10),
            ts("2026-07-07T07:00:00Z"),
        );
        t.arm(
            "s",
            TimerKind::Release,
            SignedDuration::from_secs(30),
            ts("2026-07-07T07:00:00Z"),
        );
        assert_eq!(t.len(), 1);
        let fires = t.fire_due(ts("2026-07-07T07:00:30Z"));
        assert_eq!(fires[0].kind, TimerKind::Release);
    }
}

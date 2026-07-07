//! Cron expressions (5-field, numeric) and the timer heap (spec §9).
//! Vixie semantics where defined: day-of-month OR day-of-week when both
//! are restricted; `7` == Sunday in the day-of-week field. Values are
//! numeric only in v1 — names (`MON`, `JAN`) are rejected with an error
//! naming the field.

/// One parsed field: which values are allowed over its legal range.
/// `restricted` is `false` exactly when the field text was `"*"` — the
/// distinction the vixie day-of-month/day-of-week OR rule depends on
/// (`*/n` IS restricted).
#[derive(Debug, Clone, PartialEq)]
struct FieldSet {
    min: u8,
    allowed: Vec<bool>, // index = value - min
    restricted: bool,
}

impl FieldSet {
    fn contains(&self, value: u8) -> bool {
        value
            .checked_sub(self.min)
            .is_some_and(|i| self.allowed.get(usize::from(i)).copied().unwrap_or(false))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CronExpr {
    source: String,
    minutes: FieldSet,       // 0-59
    hours: FieldSet,         // 0-23
    days_of_month: FieldSet, // 1-31
    months: FieldSet,        // 1-12
    days_of_week: FieldSet,  // 0-6, 0 = Sunday (7 normalized on parse)
}

impl CronExpr {
    /// Parse a 5-field cron expression. The error string names the field
    /// ("minute", "hour", "day of month", "month", "day of week") and the
    /// offending item; callers add the order context (spec §9: parse errors
    /// name the order and the field).
    pub fn parse(expr: &str) -> Result<CronExpr, String> {
        let fields: Vec<&str> = expr.split_whitespace().collect();
        if fields.len() != 5 {
            return Err(format!(
                "expected 5 fields (minute hour day-of-month month day-of-week), got {}",
                fields.len()
            ));
        }
        Ok(CronExpr {
            source: expr.to_owned(),
            minutes: parse_field(fields[0], "minute", 0, 59, false)?,
            hours: parse_field(fields[1], "hour", 0, 23, false)?,
            days_of_month: parse_field(fields[2], "day of month", 1, 31, false)?,
            months: parse_field(fields[3], "month", 1, 12, false)?,
            days_of_week: parse_field(fields[4], "day of week", 0, 7, true)?,
        })
    }

    /// The expression text as written in camp.toml.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Test-only: a clone with the source text replaced, so normalization
    /// equivalences (7 == Sunday) can be asserted with `PartialEq`.
    #[cfg(test)]
    fn with_source(&self, source: &str) -> CronExpr {
        let mut clone = self.clone();
        clone.source = source.to_owned();
        clone
    }
}

/// How far `next_after` searches before declaring an expression dead
/// (plan Decision N): covers the worst legal gap, `0 0 29 2 *` across a
/// leap cycle. The year-2100 century gap is outside v1's service life.
const SEARCH_HORIZON_DAYS: i32 = 366 * 6;

impl CronExpr {
    /// The earliest instant strictly after `after` matching this expression
    /// in `tz`. Nonexistent civil times (DST spring-forward gap) resolve
    /// forward and fire once; ambiguous ones (fall-back fold) fire at the
    /// first occurrence only (jiff `Disambiguation::Compatible`); any
    /// resolution ≤ `after` — possible when `after` sits in a fold's second
    /// pass — is skipped, so the result is strictly monotonic. `None` = no
    /// fire within the search horizon.
    pub fn next_after(
        &self,
        after: jiff::Timestamp,
        tz: &jiff::tz::TimeZone,
    ) -> Option<jiff::Timestamp> {
        let zoned_after = after.to_zoned(tz.clone());
        let start_date = zoned_after.date();
        let mut date = start_date;
        for _ in 0..SEARCH_HORIZON_DAYS {
            if self.day_matches(date)
                && let Some(ts) = self.first_fire_on(date, start_date, &zoned_after, after, tz)
            {
                return Some(ts);
            }
            date = date.tomorrow().ok()?; // ran off the calendar: no fire
        }
        None
    }

    /// The earliest matching instant on `date` that resolves strictly after
    /// `after`, if any.
    fn first_fire_on(
        &self,
        date: jiff::civil::Date,
        start_date: jiff::civil::Date,
        zoned_after: &jiff::Zoned,
        after: jiff::Timestamp,
        tz: &jiff::tz::TimeZone,
    ) -> Option<jiff::Timestamp> {
        for hour in 0..=23u8 {
            if !self.hours.contains(hour) {
                continue;
            }
            for minute in 0..=59u8 {
                if !self.minutes.contains(minute) {
                    continue;
                }
                let Ok(time) = jiff::civil::Time::new(hour as i8, minute as i8, 0, 0) else {
                    continue; // unreachable: hour/minute are range-checked
                };
                let candidate = jiff::civil::DateTime::from_parts(date, time);
                // Cheap civil-level cut on the first day: only candidates
                // past `after`'s local time can be fires.
                if date == start_date && candidate <= zoned_after.datetime() {
                    continue;
                }
                let Ok(zoned) = tz.to_ambiguous_zoned(candidate).compatible() else {
                    continue; // no resolution: not a fire
                };
                let ts = zoned.timestamp();
                if ts > after {
                    return Some(ts);
                }
                // fold second-pass edge: resolution ≤ after — keep searching
            }
        }
        None
    }

    /// Vixie day rule: the month must match; if BOTH day-of-month and
    /// day-of-week are restricted, either may match; if one is restricted,
    /// it decides; if neither, all days match.
    fn day_matches(&self, date: jiff::civil::Date) -> bool {
        let month = u8::try_from(date.month()).unwrap_or(0);
        if !self.months.contains(month) {
            return false;
        }
        let dom = u8::try_from(date.day()).unwrap_or(0);
        let dow = u8::try_from(date.weekday().to_sunday_zero_offset()).unwrap_or(7);
        match (self.days_of_month.restricted, self.days_of_week.restricted) {
            (true, true) => self.days_of_month.contains(dom) || self.days_of_week.contains(dow),
            (true, false) => self.days_of_month.contains(dom),
            (false, true) => self.days_of_week.contains(dow),
            (false, false) => true,
        }
    }
}

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};

use crate::error::CoreError;
use crate::orders::{Order, Trigger};

/// A fire within this much of its scheduled time is on-time (one cron
/// granule); later than this it is *missed*, and the catch-up window rule
/// (spec §9) decides whether it fires late — once, flagged — or is skipped.
pub const ON_TIME_TOLERANCE: SignedDuration = SignedDuration::from_secs(60);

/// A due fire popped by `fire_due` (plan Decision B): the order's name,
/// the scheduled instant, and whether this is a late catch-up fire. The
/// caller resolves the name back to its `Order`.
#[derive(Debug, Clone, PartialEq)]
pub struct Fire {
    pub order: String,
    pub scheduled: Timestamp,
    pub catch_up: bool,
}

/// A missed-while-not-watching fire recovered by `recompute` — always a
/// catch-up (master-plan pinned shape).
#[derive(Debug, Clone, PartialEq)]
pub struct CatchUp {
    pub order: String,
    pub scheduled: Timestamp,
}

/// Min-heap of (next fire, order index). The earliest deadline is campd's
/// poll timeout; an empty heap means infinite wait (invariant 1 — a timer,
/// never a tick).
pub struct CronHeap {
    tz: TimeZone,
    orders: Vec<Order>,
    entries: BinaryHeap<Reverse<(Timestamp, usize)>>,
}

impl CronHeap {
    pub fn new(tz: TimeZone) -> Self {
        CronHeap {
            tz,
            orders: Vec::new(),
            entries: BinaryHeap::new(),
        }
    }

    /// Arm a cron order. A non-cron order or an expression with no fire
    /// inside the search horizon is an error naming the order (fail fast:
    /// dead automation is config junk).
    pub fn arm(&mut self, order: Order, now: Timestamp) -> Result<(), CoreError> {
        let Trigger::Cron { expr } = &order.trigger else {
            return Err(CoreError::Order {
                order: order.name.clone(),
                reason: "only cron orders arm the timer heap".into(),
            });
        };
        let next = expr
            .next_after(now, &self.tz)
            .ok_or_else(|| CoreError::Order {
                order: order.name.clone(),
                reason: format!(
                    "cron expression {:?} never fires within the {SEARCH_HORIZON_DAYS}-day search horizon",
                    expr.source(),
                ),
            })?;
        let idx = self.orders.len();
        self.orders.push(order);
        self.entries.push(Reverse((next, idx)));
        Ok(())
    }

    /// The earliest armed deadline — campd's poll timeout source.
    pub fn next_deadline(&self) -> Option<Timestamp> {
        self.entries.peek().map(|Reverse((t, _))| *t)
    }

    /// Pop everything due at `now`, reschedule each from `now`, and return
    /// the fires the caller must declare. Applies the catch-up rule (plan
    /// Decision F), so a poll that overslept a system sleep behaves exactly
    /// like a detected wall-clock jump.
    pub fn fire_due(&mut self, now: Timestamp) -> Vec<Fire> {
        let mut fires = Vec::new();
        while let Some(Reverse((deadline, idx))) = self.entries.peek().copied() {
            if deadline > now {
                break;
            }
            self.entries.pop();
            let order = &self.orders[idx];
            let lateness = now.duration_since(deadline);
            if lateness <= ON_TIME_TOLERANCE {
                fires.push(Fire {
                    order: order.name.clone(),
                    scheduled: deadline,
                    catch_up: false,
                });
            } else if window_allows(order.catch_up_window, lateness) {
                fires.push(Fire {
                    order: order.name.clone(),
                    scheduled: deadline,
                    catch_up: true,
                });
            } // else: missed outside the window — skip; reschedule only
            // A None next_after here: the expression ran off the horizon
            // after years of service — the order goes quiet and
            // `camp order ls` shows "never". Documented, not hidden.
            if let Trigger::Cron { expr } = &self.orders[idx].trigger
                && let Some(next) = expr.next_after(now, &self.tz)
            {
                self.entries.push(Reverse((next, idx)));
            }
        }
        fires
    }

    /// Wall-clock jump handling (spec §9): reschedule every order from
    /// `now` and return one catch-up per order whose most recent missed
    /// fire in `(last_seen, now]` is within its window ("fire once on
    /// wake"). Scans only the window span, so a year-long gap costs
    /// window-sized work. Backward jumps (`now ≤ last_seen`) reschedule
    /// without catch-ups — wall-clock deadlines remain valid.
    pub fn recompute(&mut self, now: Timestamp, last_seen: Timestamp) -> Vec<CatchUp> {
        self.entries.clear();
        let mut catch_ups = Vec::new();
        for (idx, order) in self.orders.iter().enumerate() {
            let Trigger::Cron { expr } = &order.trigger else {
                continue;
            };
            if now > last_seen
                && let Some(scheduled) =
                    most_recent_missed(expr, order.catch_up_window, now, last_seen, &self.tz)
            {
                catch_ups.push(CatchUp {
                    order: order.name.clone(),
                    scheduled,
                });
            }
            if let Some(next) = expr.next_after(now, &self.tz) {
                self.entries.push(Reverse((next, idx)));
            }
        }
        catch_ups
    }
}

/// The most recent fire of `expr` in `(last_seen, now]` that is still
/// inside the catch-up window (or on-time tolerance), if any.
fn most_recent_missed(
    expr: &CronExpr,
    window: std::time::Duration,
    now: Timestamp,
    last_seen: Timestamp,
    tz: &TimeZone,
) -> Option<Timestamp> {
    let signed_window = SignedDuration::try_from(window).ok()?;
    if signed_window <= SignedDuration::ZERO {
        return None; // "0" disables catch-up
    }
    // Earliest instant that could still be in-window; never before last_seen.
    let floor = now
        .checked_sub(signed_window)
        .and_then(|t| t.checked_sub(ON_TIME_TOLERANCE))
        .unwrap_or(last_seen);
    let mut cursor = if floor > last_seen { floor } else { last_seen };
    let mut most_recent = None;
    while let Some(fire) = expr.next_after(cursor, tz) {
        if fire > now {
            break;
        }
        let lateness = now.duration_since(fire);
        if lateness <= ON_TIME_TOLERANCE || window_allows(window, lateness) {
            most_recent = Some(fire);
        }
        cursor = fire;
    }
    most_recent
}

fn window_allows(window: std::time::Duration, lateness: SignedDuration) -> bool {
    match SignedDuration::try_from(window) {
        Ok(w) => w > SignedDuration::ZERO && lateness <= w,
        Err(_) => false, // a window beyond SignedDuration range: treat as disabled
    }
}

/// Parse one field: comma-separated items of `*[/step]` or `a[-b][/step]`.
/// `wrap_seven`: the day-of-week field accepts 7 as an alias for 0 (Sunday);
/// its `allowed` store is still indexed 0-6.
fn parse_field(
    text: &str,
    name: &str,
    min: u8,
    max: u8,
    wrap_seven: bool,
) -> Result<FieldSet, String> {
    let store = if wrap_seven {
        usize::from(max - min) // 0-6: 7 aliases 0
    } else {
        usize::from(max - min) + 1
    };
    let mut set = FieldSet {
        min,
        allowed: vec![false; store],
        restricted: text != "*",
    };
    for item in text.split(',') {
        if item.is_empty() {
            return Err(format!("{name}: empty list item in {text:?}"));
        }
        let (range, step) = match item.split_once('/') {
            Some((r, s)) => {
                let step: u8 = s
                    .parse()
                    .map_err(|_| format!("{name}: bad step {s:?} in {item:?}"))?;
                if step == 0 {
                    return Err(format!("{name}: step 0 in {item:?}"));
                }
                (r, step)
            }
            None => (item, 1),
        };
        let (lo, hi) = if range == "*" {
            (min, max)
        } else {
            match range.split_once('-') {
                Some((a, b)) => (
                    parse_value(a, name, min, max)?,
                    parse_value(b, name, min, max)?,
                ),
                None => {
                    let v = parse_value(range, name, min, max)?;
                    (v, v)
                }
            }
        };
        if lo > hi {
            return Err(format!("{name}: inverted range {range:?}"));
        }
        let mut v = lo;
        loop {
            let normalized = if wrap_seven && v == 7 { 0 } else { v };
            let index = usize::from(normalized - min);
            if let Some(slot) = set.allowed.get_mut(index) {
                *slot = true;
            }
            match v.checked_add(step) {
                Some(next) if next <= hi => v = next,
                _ => break,
            }
        }
    }
    Ok(set)
}

fn parse_value(text: &str, name: &str, min: u8, max: u8) -> Result<u8, String> {
    let v: u8 = text
        .parse()
        .map_err(|_| format!("{name}: {text:?} is not a number (names are not supported)"))?;
    if v < min || v > max {
        return Err(format!("{name}: value {v} out of range {min}-{max}"));
    }
    Ok(v)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_spec_section_9_example() {
        // "0 7 * * 1-5" — weekday mornings at 07:00
        let expr = CronExpr::parse("0 7 * * 1-5").unwrap();
        assert_eq!(expr.source(), "0 7 * * 1-5");
    }

    #[test]
    fn accepts_lists_ranges_steps_and_wildcards() {
        for ok in [
            "* * * * *",
            "*/15 * * * *",
            "0,30 8-17 * * *",
            "5 0 1,15 1-6/2 *",
            "0 0 * * 7", // 7 == Sunday, normalized to 0
            "59 23 31 12 6",
        ] {
            CronExpr::parse(ok).unwrap_or_else(|e| panic!("{ok:?} rejected: {e}"));
        }
    }

    #[test]
    fn seven_normalizes_to_sunday() {
        assert_eq!(
            CronExpr::parse("0 0 * * 7").unwrap(),
            CronExpr::parse("0 0 * * 0")
                .unwrap()
                .with_source("0 0 * * 7")
        );
    }

    use jiff::Timestamp;
    use jiff::tz::TimeZone;

    fn ny() -> TimeZone {
        TimeZone::get("America/New_York").unwrap()
    }

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn next(expr: &str, after: &str, tz: &TimeZone) -> Option<String> {
        CronExpr::parse(expr)
            .unwrap()
            .next_after(ts(after), tz)
            .map(|t| t.to_string())
    }

    #[test]
    fn next_fire_table_utc() {
        let utc = TimeZone::UTC;
        for (expr, after, expect) in [
            // strictly after: an exact hit advances to the next match
            ("0 7 * * *", "2026-07-06T07:00:00Z", "2026-07-07T07:00:00Z"),
            ("0 7 * * *", "2026-07-06T06:59:59Z", "2026-07-06T07:00:00Z"),
            // weekday constraint: Fri 2026-07-10 19:00 → Mon 2026-07-13 07:00
            (
                "0 7 * * 1-5",
                "2026-07-10T19:00:00Z",
                "2026-07-13T07:00:00Z",
            ),
            // dom/dow OR rule (both restricted): the 15th OR a Monday
            ("0 0 15 * 1", "2026-07-10T00:00:00Z", "2026-07-13T00:00:00Z"),
            ("0 0 15 * 1", "2026-07-13T00:00:00Z", "2026-07-15T00:00:00Z"),
            // month-end: only months with a 31st
            ("0 0 31 * *", "2026-01-31T00:00:01Z", "2026-03-31T00:00:00Z"),
            // leap day: next Feb 29 after 2026 is 2028
            ("0 0 29 2 *", "2026-03-01T00:00:00Z", "2028-02-29T00:00:00Z"),
            // steps
            (
                "*/15 * * * *",
                "2026-07-06T07:41:00Z",
                "2026-07-06T07:45:00Z",
            ),
        ] {
            assert_eq!(
                next(expr, after, &utc).as_deref(),
                Some(expect),
                "{expr} after {after}"
            );
        }
    }

    #[test]
    fn spring_forward_gap_fires_once_shifted_compatible() {
        // 2026-03-08 02:30 EST does not exist (02:00→03:00). Compatible
        // disambiguation shifts forward by the gap: fires 03:30 EDT = 07:30Z.
        assert_eq!(
            next("30 2 * * *", "2026-03-08T05:00:00Z", &ny()).as_deref(), // 00:00 EST
            Some("2026-03-08T07:30:00Z")
        );
    }

    #[test]
    fn fall_back_fold_fires_first_occurrence_only() {
        // 2026-11-01 01:30 happens twice (EDT 05:30Z, then EST 06:30Z).
        // Compatible picks the earlier; the second pass is not a fire.
        assert_eq!(
            next("30 1 * * *", "2026-11-01T04:00:00Z", &ny()).as_deref(), // 00:00 EDT
            Some("2026-11-01T05:30:00Z")
        );
        // ...and from within the fold's second pass (01:45 EST = 06:45Z),
        // the next fire is the NEXT day — never an instant ≤ after.
        assert_eq!(
            next("30 1 * * *", "2026-11-01T06:45:00Z", &ny()).as_deref(),
            Some("2026-11-02T06:30:00Z")
        );
    }

    #[test]
    fn impossible_dates_return_none() {
        assert_eq!(
            next("0 0 30 2 *", "2026-01-01T00:00:00Z", &TimeZone::UTC),
            None
        );
    }

    use crate::orders::{Order, Trigger};
    use std::time::Duration;

    fn cron_order(name: &str, expr: &str, window: Duration) -> Order {
        Order {
            name: name.into(),
            trigger: Trigger::Cron {
                expr: CronExpr::parse(expr).unwrap(),
            },
            formula: "f".into(),
            rig: None,
            catch_up_window: window,
        }
    }

    const TWO_HOURS: Duration = Duration::from_secs(2 * 60 * 60);

    #[test]
    fn empty_heap_has_no_deadline() {
        assert_eq!(CronHeap::new(TimeZone::UTC).next_deadline(), None);
    }

    #[test]
    fn interleaved_schedules_order_the_heap() {
        let mut heap = CronHeap::new(TimeZone::UTC);
        let now = ts("2026-07-06T07:20:00Z");
        heap.arm(cron_order("hourly", "0 * * * *", TWO_HOURS), now)
            .unwrap();
        heap.arm(cron_order("quarter", "*/15 * * * *", TWO_HOURS), now)
            .unwrap();
        // quarter fires 07:30, hourly 08:00
        assert_eq!(heap.next_deadline(), Some(ts("2026-07-06T07:30:00Z")));
        let fires = heap.fire_due(ts("2026-07-06T07:30:00Z"));
        assert_eq!(fires.len(), 1);
        assert_eq!(fires[0].order, "quarter");
        assert_eq!(fires[0].scheduled, ts("2026-07-06T07:30:00Z"));
        assert!(!fires[0].catch_up);
        // quarter rescheduled to 07:45, still ahead of hourly
        assert_eq!(heap.next_deadline(), Some(ts("2026-07-06T07:45:00Z")));
        // both due at 08:00 (quarter's 07:45 missed by 15 min → within window)
        let fires = heap.fire_due(ts("2026-07-06T08:00:00Z"));
        let names: Vec<&str> = fires.iter().map(|f| f.order.as_str()).collect();
        assert!(names.contains(&"quarter") && names.contains(&"hourly"));
    }

    #[test]
    fn fire_due_is_empty_before_the_deadline() {
        let mut heap = CronHeap::new(TimeZone::UTC);
        heap.arm(
            cron_order("h", "0 * * * *", TWO_HOURS),
            ts("2026-07-06T07:20:00Z"),
        )
        .unwrap();
        assert!(heap.fire_due(ts("2026-07-06T07:59:59Z")).is_empty());
        assert_eq!(heap.next_deadline(), Some(ts("2026-07-06T08:00:00Z")));
    }

    #[test]
    fn late_fire_within_window_is_a_catch_up_fire() {
        let mut heap = CronHeap::new(TimeZone::UTC);
        heap.arm(
            cron_order("h", "0 8 * * *", TWO_HOURS),
            ts("2026-07-06T07:00:00Z"),
        )
        .unwrap();
        // wakes 90 min late (poll timeout ticked through a sleep)
        let fires = heap.fire_due(ts("2026-07-06T09:30:00Z"));
        assert_eq!(fires.len(), 1);
        assert!(fires[0].catch_up);
        assert_eq!(fires[0].scheduled, ts("2026-07-06T08:00:00Z"));
        // rescheduled from now: next fire tomorrow 08:00
        assert_eq!(heap.next_deadline(), Some(ts("2026-07-07T08:00:00Z")));
    }

    #[test]
    fn late_fire_outside_window_is_skipped_and_rescheduled() {
        let mut heap = CronHeap::new(TimeZone::UTC);
        heap.arm(
            cron_order("h", "0 8 * * *", TWO_HOURS),
            ts("2026-07-06T07:00:00Z"),
        )
        .unwrap();
        assert!(heap.fire_due(ts("2026-07-06T10:00:01Z")).is_empty()); // 2h1s late
        assert_eq!(heap.next_deadline(), Some(ts("2026-07-07T08:00:00Z")));
    }

    #[test]
    fn zero_window_disables_catch_up_but_not_on_time_fires() {
        let mut heap = CronHeap::new(TimeZone::UTC);
        heap.arm(
            cron_order("h", "0 8 * * *", Duration::ZERO),
            ts("2026-07-06T07:00:00Z"),
        )
        .unwrap();
        // 30 s late is within ON_TIME_TOLERANCE: a normal fire
        let fires = heap.fire_due(ts("2026-07-06T08:00:30Z"));
        assert_eq!(fires.len(), 1);
        assert!(!fires[0].catch_up);
        // next day, 10 min late: beyond tolerance, window disabled → skip
        assert!(heap.fire_due(ts("2026-07-07T08:10:00Z")).is_empty());
        assert_eq!(heap.next_deadline(), Some(ts("2026-07-08T08:00:00Z")));
    }

    #[test]
    fn recompute_fires_once_with_the_most_recent_missed_fire() {
        let mut heap = CronHeap::new(TimeZone::UTC);
        let armed = ts("2026-07-06T06:30:00Z");
        heap.arm(cron_order("hourly", "0 * * * *", TWO_HOURS), armed)
            .unwrap();
        // slept 06:30 → 09:30: missed 07:00, 08:00, 09:00; 08:00+09:00 in window
        let catch_ups = heap.recompute(ts("2026-07-06T09:30:00Z"), armed);
        assert_eq!(catch_ups.len(), 1);
        assert_eq!(catch_ups[0].order, "hourly");
        assert_eq!(catch_ups[0].scheduled, ts("2026-07-06T09:00:00Z")); // most recent
        assert_eq!(heap.next_deadline(), Some(ts("2026-07-06T10:00:00Z")));
    }

    #[test]
    fn recompute_outside_window_and_zero_window_yield_no_catch_ups() {
        let mut heap = CronHeap::new(TimeZone::UTC);
        let armed = ts("2026-07-06T06:30:00Z");
        heap.arm(cron_order("daily", "0 7 * * *", TWO_HOURS), armed)
            .unwrap();
        heap.arm(cron_order("zeroed", "0 8 * * *", Duration::ZERO), armed)
            .unwrap();
        // woke at 12:00: 07:00 is 5h late (outside 2h), 08:00 window disabled
        let catch_ups = heap.recompute(ts("2026-07-06T12:00:00Z"), armed);
        assert!(catch_ups.is_empty());
        assert_eq!(heap.next_deadline(), Some(ts("2026-07-07T07:00:00Z")));
    }

    #[test]
    fn recompute_on_backward_jump_reschedules_without_catch_ups() {
        let mut heap = CronHeap::new(TimeZone::UTC);
        let armed = ts("2026-07-06T07:30:00Z");
        heap.arm(cron_order("h", "0 * * * *", TWO_HOURS), armed)
            .unwrap();
        let catch_ups = heap.recompute(ts("2026-07-06T06:00:00Z"), armed); // clock set back
        assert!(catch_ups.is_empty());
        assert_eq!(heap.next_deadline(), Some(ts("2026-07-06T07:00:00Z")));
    }

    #[test]
    fn arming_a_never_firing_expression_is_an_error_naming_the_order() {
        let mut heap = CronHeap::new(TimeZone::UTC);
        let err = heap
            .arm(
                cron_order("dead", "0 0 30 2 *", TWO_HOURS),
                ts("2026-07-06T07:00:00Z"),
            )
            .unwrap_err();
        assert!(err.to_string().contains("dead"), "{err}");
        assert!(err.to_string().contains("never fires"), "{err}");
    }

    #[test]
    fn arming_an_event_order_is_an_error() {
        let mut heap = CronHeap::new(TimeZone::UTC);
        let order = Order {
            name: "ev".into(),
            trigger: Trigger::Event {
                event_type: "bead.closed".into(),
                label: None,
            },
            formula: "f".into(),
            rig: None,
            catch_up_window: TWO_HOURS,
        };
        assert!(heap.arm(order, ts("2026-07-06T07:00:00Z")).is_err());
    }

    #[test]
    fn rejects_with_the_field_named() {
        for (expr, field) in [
            ("0 7 * *", "expected 5 fields"), // arity
            ("0 7 * * 1-5 9", "expected 5 fields"),
            ("60 * * * *", "minute"),
            ("* 24 * * *", "hour"),
            ("* * 0 * *", "day of month"),
            ("* * 32 * *", "day of month"),
            ("* * * 13 *", "month"),
            ("* * * 0 *", "month"),
            ("* * * * 8", "day of week"),
            ("* * * * MON", "day of week"), // names rejected in v1
            ("*/0 * * * *", "minute"),      // zero step
            ("5-1 * * * *", "minute"),      // inverted range
            ("1,,2 * * * *", "minute"),     // empty list item
            ("", "expected 5 fields"),
        ] {
            let err = CronExpr::parse(expr).unwrap_err();
            assert!(
                err.contains(field),
                "{expr:?}: error {err:?} must name {field:?}"
            );
        }
    }
}

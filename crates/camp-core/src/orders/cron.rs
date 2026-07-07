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

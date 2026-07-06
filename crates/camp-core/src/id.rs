//! Per-rig bead id allocation (spec §12). Ids are `{prefix}-{n}` with a
//! monotonic per-prefix counter that is *folded state*: `bead.created` bumps
//! the `counters` table, so a refold reconstructs the exact allocation
//! high-water mark from history and `doctor --refold` stays exact.

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::CoreError;

/// A prefix is a lowercase letter followed by lowercase alphanumerics. No
/// hyphens: an id splits on its first '-', so the prefix must not contain one.
pub fn validate_prefix(prefix: &str) -> Result<(), CoreError> {
    let mut chars = prefix.chars();
    let ok = matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    if ok {
        Ok(())
    } else {
        Err(CoreError::InvalidPrefix(prefix.to_owned()))
    }
}

/// Split an id into `(prefix, number)`. `None` if it is not a well-formed,
/// canonical camp bead id (no leading zeros, valid prefix, non-negative int).
pub fn parse_bead_id(id: &str) -> Option<(&str, i64)> {
    let (prefix, num) = id.split_once('-')?;
    if num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if num.len() > 1 && num.starts_with('0') {
        return None; // non-canonical
    }
    validate_prefix(prefix).ok()?;
    let n: i64 = num.parse().ok()?;
    Some((prefix, n))
}

/// The next unused id for `prefix`, e.g. `gc-143`. Reads the folded counter;
/// the caller appends `bead.created` with this id and the fold advances the
/// counter to match (decision E).
pub fn next_bead_id(conn: &Connection, prefix: &str) -> Result<String, CoreError> {
    validate_prefix(prefix)?;
    let high: i64 = conn
        .query_row("SELECT high FROM counters WHERE prefix = ?1", [prefix], |r| {
            r.get(0)
        })
        .optional()?
        .unwrap_or(0);
    Ok(format!("{prefix}-{}", high + 1))
}

/// Fold effect of `bead.created`: raise the prefix counter to at least this
/// id's number. Called from `fold::bead_created` inside the write txn.
pub(crate) fn bump_counter(conn: &Connection, id: &str) -> Result<(), CoreError> {
    let (prefix, n) = parse_bead_id(id).ok_or_else(|| CoreError::InvalidEventData {
        event_type: "bead.created".to_owned(),
        reason: format!("bead id {id:?} is not a well-formed {{prefix}}-{{n}} id"),
    })?;
    conn.execute(
        "INSERT INTO counters (prefix, high) VALUES (?1, ?2)
         ON CONFLICT(prefix) DO UPDATE SET high = max(high, excluded.high)",
        params![prefix, n],
    )?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn validate_prefix_accepts_and_rejects() {
        for good in ["gc", "t3", "gascity", "a0"] {
            assert!(validate_prefix(good).is_ok(), "{good} should be valid");
        }
        for bad in ["", "3d", "GC", "g-c", "g_c", "g c"] {
            assert!(validate_prefix(bad).is_err(), "{bad} should be invalid");
        }
    }

    #[test]
    fn parse_bead_id_rules() {
        assert_eq!(parse_bead_id("gc-142"), Some(("gc", 142)));
        assert_eq!(parse_bead_id("gc-0"), Some(("gc", 0)));
        assert_eq!(parse_bead_id("t3-17"), Some(("t3", 17)));
        for bad in ["gc", "gc-", "-1", "gc-x", "gc-01", "GC-1", "3d-1", "gc-1-2"] {
            // "gc-1-2" splits to ("gc","1-2") -> non-digit -> None
            assert_eq!(parse_bead_id(bad), None, "{bad} should not parse");
        }
    }
}

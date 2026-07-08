//! Health patrol state machines (spec §10): pure, deterministic, no I/O.
//! Durations are jiff friendly strings ("10m"); the pure machines take
//! explicit `now: Timestamp` (the CronHeap precedent). Patrol config is
//! read at campd start; hot reload does not re-arm patrol (Phase 11 plan
//! Decision L).

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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use jiff::SignedDuration;

    #[test]
    fn parse_duration_accepts_friendly_forms() {
        assert_eq!(parse_duration("10m").unwrap(), SignedDuration::from_mins(10));
        assert_eq!(parse_duration("90s").unwrap(), SignedDuration::from_secs(90));
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
}

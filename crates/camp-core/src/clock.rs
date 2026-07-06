/// Source of event timestamps: RFC3339 UTC, whole seconds, e.g.
/// "2026-07-05T21:14:03Z" (the spec §7.2 canonical form). `seq` is the
/// authoritative order; `ts` is informational.
pub trait Clock: Send {
    fn now_utc(&self) -> String;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now_utc(&self) -> String {
        jiff::Timestamp::now()
            .strftime("%Y-%m-%dT%H:%M:%SZ")
            .to_string()
    }
}

/// Deterministic clock for tests.
pub struct FixedClock(String);

impl FixedClock {
    pub fn new(ts: impl Into<String>) -> Self {
        Self(ts.into())
    }
}

impl Clock for FixedClock {
    fn now_utc(&self) -> String {
        self.0.clone()
    }
}

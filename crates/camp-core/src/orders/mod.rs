//! Orders (spec §9): cron- and event-triggered formulas. The cron machinery
//! is a timer heap, never a tick (invariant 1). Grows over the Phase 10
//! tasks: cron engine and heap (`cron`), `[[order]]` compilation (`parse`),
//! and the fire pipeline (here).

pub mod cron;

use cron::CronExpr;

/// What trips an order (spec §9): a cron schedule or an event pattern.
#[derive(Debug, Clone, PartialEq)]
pub enum Trigger {
    Cron { expr: CronExpr },
    Event {
        event_type: String,
        label: Option<String>,
    },
}

/// One compiled `[[order]]` table (spec §9).
#[derive(Debug, Clone, PartialEq)]
pub struct Order {
    pub name: String,
    pub trigger: Trigger,
    pub formula: String,
    pub rig: Option<String>,
    /// Missed-fire catch-up window (spec §9): default 2h; `Duration::ZERO`
    /// (config `"0"`) disables catch-up.
    pub catch_up_window: std::time::Duration,
}

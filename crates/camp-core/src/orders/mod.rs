//! Orders (spec §9): cron- and event-triggered formulas. The cron machinery
//! is a timer heap, never a tick (invariant 1). Grows over the Phase 10
//! tasks: cron engine and heap (`cron`), `[[order]]` compilation (`parse`),
//! and the fire pipeline (here).

pub mod cron;

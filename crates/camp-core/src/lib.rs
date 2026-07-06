#![forbid(unsafe_code)]
//! camp-core: the Gas Camp ledger and pure logic. No process spawning here.

pub mod clock;
pub mod config;
pub mod error;
pub mod event;
pub mod id;
pub mod ledger;
pub mod vocab;

/// Monotonic event sequence number (the `events.seq` column).
pub type Seq = i64;

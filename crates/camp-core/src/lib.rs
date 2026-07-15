#![forbid(unsafe_code)]
//! camp-core: the Gas Camp ledger and pure logic. No process spawning here.

pub mod clock;
pub mod config;
pub mod error;
pub mod event;
pub mod export;
pub mod formula;
pub mod id;
pub mod import;
pub mod ledger;
pub mod mail;
pub mod orders;
pub mod pack;
pub mod patrol;
pub mod promptsafe;
pub mod readiness;
pub mod search;
pub mod vocab;

pub use readiness::{BeadRow, ListFilter};
pub use search::SearchHit;

/// Monotonic event sequence number (the `events.seq` column).
pub type Seq = i64;

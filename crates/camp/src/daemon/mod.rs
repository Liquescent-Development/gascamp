//! campd: the only standing process (spec §5). Crash-only: no exclusive
//! state, `kill -9` is a supported shutdown method.

pub mod socket;

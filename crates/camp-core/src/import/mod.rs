//! Pack import machinery (compat §7): the binding namespace. The umbrella
//! spec's compat phase 1 replaces the `packs = [...]` list with explicit
//! imports — each materialized under `<root>/imports/<binding>/` and
//! qualified as `<binding>.<name>`.
//!
//! This module is the pure camp-core half: source grammar, the lock model,
//! the pack manifest, materialization, transitive resolution, skills
//! install, and the `trust_exec` inventory. The camp binary half (`camp
//! import` verbs + the hardened git subprocess) lives in `crates/camp/src/cmd/import.rs`.

pub mod source;
//! compat §6 — the gc/bd shims. campd installs two `#!/bin/sh` argv-translators
//! into `.camp/bin` (see [`install`]) that `exec` camp's own absolute binary as
//! `camp gc-shim …` / `camp bd-shim …`. These entry points are the SOLE new
//! ledger-touching surface; `camp` stays the one process that writes `camp.db`.
//!
//! Exit-code channel: a shim's outcome is a process exit code the normal
//! `report()` wrapper cannot express. `0` = success/work; `1` = drain (a NORMAL
//! outcome, not an error). So `main` calls these directly and converts
//! [`ShimExit`] to an exit code, bypassing `report()`.

pub mod install;

use anyhow::{Result, bail};

use crate::campdir::CampDir;

/// The process exit code a shim intends. NOT an error channel: a genuine error
/// is `Err` (the `main` arm prints it and exits 1); a drain is `Ok(ShimExit(1))`
/// (a normal outcome, printed as JSON, no error text).
pub struct ShimExit(pub u8);

/// `camp gc-shim <verb> …` — the `gc` translator entry point.
pub fn gc_shim(camp: &CampDir, args: Vec<String>) -> Result<ShimExit> {
    let _ = camp;
    let verb = args.first().map(String::as_str).unwrap_or("");
    // Task 5 replaces this with the verb match + evented refusal; Tasks 6/8 add
    // the served verbs.
    bail!("gc shim: unimplemented verb {verb:?}")
}

/// `camp bd-shim <verb> …` — the `bd` translator entry point.
pub fn bd_shim(camp: &CampDir, args: Vec<String>) -> Result<ShimExit> {
    let _ = camp;
    let verb = args.first().map(String::as_str).unwrap_or("");
    bail!("bd shim: unimplemented verb {verb:?}")
}

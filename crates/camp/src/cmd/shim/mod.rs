//! compat §6 — the gc/bd shims. campd installs two `#!/bin/sh` argv-translators
//! into `.camp/bin` (see [`install`]) that `exec` camp's own absolute binary as
//! `camp gc-shim …` / `camp bd-shim …`. These entry points are the SOLE new
//! ledger-touching surface; `camp` stays the one process that writes `camp.db`.
//!
//! Exit-code channel: a shim's outcome is a process exit code the normal
//! `report()` wrapper cannot express. `0` = success/work; `1` = drain (a NORMAL
//! outcome, not an error). So `main` calls these directly and converts
//! [`ShimExit`] to an exit code, bypassing `report()`.

pub mod bd;
pub mod hook;
pub mod install;
pub mod mail;
pub mod prime;
pub mod project;
pub mod runtime;

use anyhow::{Result, bail};
use camp_core::event::{EventInput, EventType};
use camp_core::ledger::Ledger;

use crate::campdir::CampDir;

/// The process exit code a shim intends. NOT an error channel: a genuine error
/// is `Err` (the `main` arm prints it and exits 1); a drain is `Ok(ShimExit(1))`
/// (a normal outcome, printed as JSON, no error text).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShimExit(pub u8);

/// FAIL FAST + EVENTED (§6): a shim handed a verb or flag camp does not serve
/// appends `shim.refused` (NAMING the verb — invariant 3) AND returns `Err`.
/// Never a no-op: "a silently-ignored `bd update --set-metadata gc.outcome=pass`
/// is a corrupted ledger". Called by the catch-all verb arm below AND by each
/// served handler on an unknown flag (Tasks 6–8).
pub fn refuse(camp: &CampDir, verb: &str, detail: &str) -> Result<ShimExit> {
    // Attribute the refusal to the worker. camp exports `GC_AGENT` = the
    // qualified agent (Task 9); its dotted prefix is the pack binding. An
    // unattributable refusal (no env) is still recorded — binding/agent null.
    let agent = std::env::var("GC_AGENT").ok();
    let binding = agent
        .as_deref()
        .and_then(|a| a.split_once('.').map(|(b, _)| b.to_owned()));
    let mut ledger = Ledger::open(&camp.db_path())?;
    let seq = ledger.append(EventInput {
        kind: EventType::ShimRefused,
        rig: None,
        actor: "gc-shim".into(),
        bead: None,
        data: serde_json::json!({
            "binding": binding,
            "agent": agent,
            "verb": verb,
            "detail": detail,
        }),
    })?;
    crate::daemon::socket::poke_best_effort(camp, seq);
    bail!("gc/bd shim: unsupported verb {verb:?}: {detail}");
}

/// The refused subcommand word, e.g. `mol` from `gc mol list` — the same token
/// Task 1's `verbs_static` and the fragment name, so `shim.refused` greps cleanly.
fn verb_of(args: &[String]) -> String {
    args.first().cloned().unwrap_or_default()
}

/// `camp gc-shim <verb> …` — the `gc` translator entry point.
pub fn gc_shim(camp: &CampDir, args: Vec<String>) -> Result<ShimExit> {
    match args.first().map(String::as_str) {
        Some("hook") => hook::run(camp, &args[1..]),
        Some("runtime") => runtime::run_runtime(camp, &args[1..]),
        Some("convoy") => runtime::run_convoy(camp, &args[1..]),
        Some("mail") => mail::run(camp, &args[1..]),
        Some("prime") => prime::run(camp, &args[1..]),
        // Every other gc verb (mol, …) is refused loudly (§6).
        _ => refuse(camp, &verb_of(&args), "gc shim does not serve this verb"),
    }
}

/// `camp bd-shim <verb> …` — the `bd` translator entry point.
pub fn bd_shim(camp: &CampDir, args: Vec<String>) -> Result<ShimExit> {
    bd::run(camp, &args)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use camp_core::event::{Event, EventType};
    use camp_core::ledger::Ledger;

    fn temp_camp() -> (tempfile::TempDir, CampDir) {
        let dir = tempfile::tempdir().unwrap();
        let camp = CampDir {
            root: dir.path().to_path_buf(),
        };
        drop(Ledger::open(&camp.db_path()).unwrap());
        (dir, camp)
    }

    fn read_events(camp: &CampDir) -> Vec<Event> {
        let ledger = Ledger::open(&camp.db_path()).unwrap();
        ledger.events_range(1, None).unwrap()
    }

    #[test]
    fn unknown_gc_verb_fails_fast_and_events_shim_refused() {
        let (_d, camp) = temp_camp();
        let err = gc_shim(&camp, vec!["mol".into(), "list".into()]).unwrap_err();
        assert!(format!("{err:#}").contains("mol"), "names the refused verb");
        assert!(
            read_events(&camp)
                .iter()
                .any(|e| e.kind == EventType::ShimRefused && e.data["verb"] == "mol"),
            "a refused verb must land a shim.refused event (§6): a silently-\
             ignored shim call is a corrupted ledger"
        );
    }

    #[test]
    fn unknown_bd_verb_fails_fast_and_events_shim_refused() {
        let (_d, camp) = temp_camp();
        let err = bd_shim(&camp, vec!["mol".into(), "current".into()]).unwrap_err();
        assert!(format!("{err:#}").contains("mol"));
        assert!(
            read_events(&camp)
                .iter()
                .any(|e| e.kind == EventType::ShimRefused && e.data["verb"] == "mol")
        );
    }

    #[test]
    fn refusal_records_binding_and_agent_fields_even_when_unattributed() {
        // With no GC_AGENT in the env, binding/agent are null — but the event
        // still records them (the shape is stable for downstream readers).
        // `mol` is a genuinely-refused verb (prime/mail are now SERVED, compat-4).
        let (_d, camp) = temp_camp();
        let _ = gc_shim(&camp, vec!["mol".into()]);
        let ev = read_events(&camp)
            .into_iter()
            .find(|e| e.kind == EventType::ShimRefused)
            .unwrap();
        assert_eq!(ev.data["verb"], "mol");
        assert!(ev.data.get("binding").is_some(), "binding key present");
        assert!(ev.data.get("agent").is_some(), "agent key present");
    }
}

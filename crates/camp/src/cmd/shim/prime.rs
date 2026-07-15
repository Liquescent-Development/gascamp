//! compat §6 verb table — `gc prime` renders the agent's prompt template to
//! stdout. campd delivers the agent's prompt RAW (`spawn.rs` --append-system-
//! prompt); `prime` is the renderer that resolves the SAME agent (via the
//! compat-1 binding namespace, `resolve_agent`) and prints its materialized
//! prompt. Name = args[0] else $GC_ALIAS else $GC_AGENT (mirrors gc's
//! `primeInvocationAgentName`, GASCITY_REF). NO default-prompt fallback: the
//! shim is dispatch-only (§6.3); an unresolvable agent is a hard error.
//!
//! The name env vars (`GC_ALIAS`/`GC_AGENT`) are read ONCE at the env edge
//! (`run`) and INJECTED into the testable core — the codebase forbids `unsafe`,
//! and edition-2024 `env::set_var` is `unsafe`, so the core is env-free
//! (invariant 5).

use anyhow::{Result, bail};

use super::ShimExit;
use crate::campdir::CampDir;

/// The env edge: read the gc name-resolution env and delegate.
pub fn run(camp: &CampDir, args: &[String]) -> Result<ShimExit> {
    let gc_alias = std::env::var("GC_ALIAS").ok();
    let gc_agent = std::env::var("GC_AGENT").ok();
    run_with_env(camp, args, gc_alias.as_deref(), gc_agent.as_deref())
}

/// The testable core: resolve the agent name (arg, else alias, else agent) and
/// print its materialized prompt. No env reads — the two fallbacks are injected.
fn run_with_env(
    camp: &CampDir,
    args: &[String],
    gc_alias: Option<&str>,
    gc_agent: Option<&str>,
) -> Result<ShimExit> {
    let name = invocation_agent_name(args, gc_alias, gc_agent);
    if name.is_empty() {
        bail!("gc prime: no agent name (args, $GC_ALIAS, or $GC_AGENT) — cannot render a prompt");
    }
    let cfg = camp_core::config::CampConfig::load(&camp.config_path())?;
    let agent = camp_core::pack::resolve_agent(&cfg, &name)?;
    print!("{}", agent.prompt);
    Ok(ShimExit(0))
}

/// Name resolution mirrors gc (A5): args[0], else $GC_ALIAS, else $GC_AGENT.
fn invocation_agent_name(
    args: &[String],
    gc_alias: Option<&str>,
    gc_agent: Option<&str>,
) -> String {
    if let Some(first) = args.iter().find(|a| !a.starts_with('-')) {
        return first.trim().to_owned();
    }
    for v in [gc_alias, gc_agent].into_iter().flatten() {
        if !v.trim().is_empty() {
            return v.trim().to_owned();
        }
    }
    String::new()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// A camp with a camp-local agent `dev` whose prompt.md is known text.
    fn camp_with_agent() -> (tempfile::TempDir, CampDir) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".camp");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("camp.toml"),
            "[camp]\nname = \"t\"\n\n[agent_defaults]\ntools = [\"Read\"]\n",
        )
        .unwrap();
        let dev = root.join("agents/dev");
        std::fs::create_dir_all(&dev).unwrap();
        std::fs::write(dev.join("prompt.md"), "You are the dev worker. Do TDD.").unwrap();
        (dir, CampDir { root })
    }

    #[test]
    fn prime_resolves_the_named_agent_and_exits_zero() {
        let (_d, camp) = camp_with_agent();
        let cfg = camp_core::config::CampConfig::load(&camp.config_path()).unwrap();
        let agent = camp_core::pack::resolve_agent(&cfg, "dev").unwrap();
        assert_eq!(agent.prompt, "You are the dev worker. Do TDD.");
        assert_eq!(
            run_with_env(&camp, &["dev".to_owned()], None, None)
                .unwrap()
                .0,
            0
        );
    }

    #[test]
    fn prime_resolves_a_qualified_gc_agent_name() {
        // The REAL worker's GC_AGENT is qualified (e.g. `gc.publisher`), NOT a
        // bare name. Mirror compat-1's import fixture (pack.rs
        // `qualified_route_resolves_through_binding`): a git-source binding
        // materializes at <root>/imports/<binding>/agents/<agent>/.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::write(
            root.join("camp.toml"),
            "[camp]\nname=\"t\"\n[agent_defaults]\ntools=[\"Read\"]\n[imports.gc]\nsource=\"file:///unused\"\n",
        )
        .unwrap();
        let a = root.join("imports/gc/agents/publisher");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::write(a.join("prompt.md"), "QUALIFIED_PRIME: publish it.").unwrap();
        let camp = CampDir { root };
        let cfg = camp_core::config::CampConfig::load(&camp.config_path()).unwrap();
        assert_eq!(
            camp_core::pack::resolve_agent(&cfg, "gc.publisher")
                .unwrap()
                .prompt,
            "QUALIFIED_PRIME: publish it."
        );
        assert_eq!(
            run_with_env(&camp, &["gc.publisher".to_owned()], None, None)
                .unwrap()
                .0,
            0
        );
    }

    #[test]
    fn prime_falls_back_to_gc_agent_env_when_no_arg() {
        let (_d, camp) = camp_with_agent();
        // Injected env value (no process-global set_var — invariant 5).
        assert_eq!(invocation_agent_name(&[], None, Some("dev")), "dev");
        assert_eq!(run_with_env(&camp, &[], None, Some("dev")).unwrap().0, 0);
    }

    #[test]
    fn prime_prefers_alias_over_agent_and_arg_over_both() {
        // gc's precedence (A5): args[0] wins, else GC_ALIAS, else GC_AGENT.
        assert_eq!(
            invocation_agent_name(&["arg".to_owned()], Some("alias"), Some("agent")),
            "arg"
        );
        assert_eq!(
            invocation_agent_name(&[], Some("alias"), Some("agent")),
            "alias"
        );
        assert_eq!(invocation_agent_name(&[], None, Some("agent")), "agent");
    }

    #[test]
    fn prime_with_no_name_anywhere_is_a_hard_error_not_a_default_prompt() {
        let (_d, camp) = camp_with_agent();
        let err = run_with_env(&camp, &[], None, None).unwrap_err();
        assert!(format!("{err:#}").contains("no agent name"));
    }

    #[test]
    fn prime_on_an_unknown_agent_fails_fast_naming_it() {
        let (_d, camp) = camp_with_agent();
        let err = run_with_env(&camp, &["ghost".to_owned()], None, None).unwrap_err();
        assert!(format!("{err:#}").contains("ghost"));
    }
}

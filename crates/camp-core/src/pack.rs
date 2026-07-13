//! Packs (compat §5.1/§7): an agent is a Gas City agent **directory** —
//! `agent.toml` (optional, unknown keys tolerated) plus a prompt file. The
//! directory name IS the agent's identity. Model/permission/tools are
//! operator-owned (`[agent_defaults]` in camp.toml, §5.2), never read from the
//! pack. §5.4 unsupported keys are collected as refusals (the agent still
//! materializes; the operator is told), and appended as `import.refused`
//! ledger events by `camp import`.
//!
//! Resolution (`resolve_agent`) keeps its pinned signature so the
//! sibling-owned consumers (`dispatch.rs`, `patrol.rs`, `sling.rs`,
//! `spawn.rs`) never need editing — only the resolution *source* changes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::{AgentDefaults, CampConfig};
use crate::error::CoreError;

/// Where a dispatched worker's tree lives (spec §12). Worktree is the
/// DEFAULT for autonomous dispatch (dispatch-lifecycle Q1, approved
/// 2026-07-09): workers never run on the rig's live branch unless the
/// agent explicitly declares `isolation = "none"` — and that opt-out is
/// loud (`dispatch.live_tree`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Isolation {
    None,
    #[default]
    Worktree,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AgentDef {
    pub name: String,
    pub model: Option<String>,
    pub tools: Option<Vec<String>>,
    pub permission_mode: Option<String>,
    pub isolation: Isolation,
    /// Per-agent stall threshold override (Phase 11, spec §10): a friendly
    /// duration string ("5m"), validated at parse. `None` uses the camp
    /// `[patrol] stall_after` default.
    pub stall_after: Option<String>,
    pub prompt: String,
}

/// A §5.4 refusal: a pack/agent key camp does not honor. Collected, not
/// thrown — the agent still materializes; the operator is told (and each
/// refusal becomes an `import.refused` ledger event).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRefusal {
    pub agent: String,
    pub key: String,
}

/// A parsed agent directory before operator defaults are applied. Identity
/// is the directory name; the prompt is the first matching prompt file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawAgent {
    pub name: String,
    pub prompt: String,
    pub scope: Option<String>,
    pub stall_after: Option<String>,
}

/// §5.4 refused keys (umbrella §5.4): camp reads none of these. They are
/// collected as `AgentRefusal`s so the operator is told, never silently
/// dropped. Model/permission/tools are NOT here — those are operator-owned
/// (§5.2) and never read from the pack.
const REFUSED_KEYS: &[&str] = &[
    "pre_start",
    "work_dir",
    "wake_mode",
    "idle_timeout",
    "min_active_sessions",
    "max_active_sessions",
    "nudge",
    "sleep_after_idle",
    "max_session_age",
    "max_session_age_jitter",
];

/// Prompt file precedence (umbrella §5.1): first existing of these.
const PROMPT_FILES: &[&str] = &["prompt.template.md", "prompt.md.tmpl", "prompt.md"];

fn pack_err(path: &Path, reason: impl std::fmt::Display) -> CoreError {
    CoreError::Pack(format!("{}: {reason}", path.display()))
}

/// Parse one agent DIRECTORY (umbrella §5.1). Identity = the directory
/// name. Prompt precedence: `prompt.template.md`, `prompt.md.tmpl`,
/// `prompt.md`. `agent.toml` is OPTIONAL, and unknown keys are TOLERATED
/// (umbrella §4 — `camp.toml`'s strictness never leaks into `agent.toml`).
/// Returns the parsed agent plus any §5.4 refusals.
pub fn parse_agent_dir(dir: &Path) -> Result<(RawAgent, Vec<AgentRefusal>), CoreError> {
    let name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| pack_err(dir, "agent directory has no name"))?
        .to_owned();

    let prompt = PROMPT_FILES
        .iter()
        .find_map(|f| {
            let p = dir.join(f);
            p.is_file().then(|| p)
        })
        .ok_or_else(|| pack_err(dir, "no prompt file (expected prompt.template.md, prompt.md.tmpl, or prompt.md)"))?;
    let prompt = std::fs::read_to_string(&prompt).map_err(|e| {
        pack_err(dir, format!("cannot read {}: {e}", prompt.display()))
    })?;
    if prompt.trim().is_empty() {
        return Err(pack_err(dir, "empty prompt — an agent must say what it does"));
    }

    let mut scope = None;
    let mut stall_after = None;
    let mut refusals = Vec::new();
    let agent_toml = dir.join("agent.toml");
    if agent_toml.is_file() {
        let text = std::fs::read_to_string(&agent_toml)
            .map_err(|e| pack_err(dir, format!("cannot read agent.toml: {e}")))?;
        let doc: toml::Value =
            toml::from_str(&text).map_err(|e| pack_err(dir, format!("agent.toml is not valid TOML: {e}")))?;
        if let Some(table) = doc.as_table() {
            for (key, value) in table {
                match key.as_str() {
                    "scope" => {
                        scope = value.as_str().map(|s| s.to_owned());
                    }
                    "stall_after" => {
                        let s = value.as_str().ok_or_else(|| {
                            pack_err(dir, "agent.toml key \"stall_after\" must be a string")
                        })?;
                        crate::patrol::parse_duration(s).map_err(|e| {
                            pack_err(dir, format!("agent.toml key \"stall_after\": {e}"))
                        })?;
                        stall_after = Some(s.to_owned());
                    }
                    k if REFUSED_KEYS.contains(&k) => {
                        refusals.push(AgentRefusal {
                            agent: name.clone(),
                            key: k.to_owned(),
                        });
                    }
                    _ => {} // tolerated (umbrella §4)
                }
            }
        }
    }

    Ok((
        RawAgent {
            name,
            prompt,
            scope,
            stall_after,
        },
        refusals,
    ))
}

/// The agents/ layers to search, lowest to highest (plan decision R).
/// Phase 1 interim: the `packs` field is gone (compat §7 — packs now import
/// under `<root>/imports/<binding>/`); the binding-qualified rewrite lands
/// in Task 12. Until then the camp-local `<root>/agents/` layer is the only one.
fn layers(cfg: &CampConfig) -> Result<Vec<PathBuf>, CoreError> {
    let need_root = || {
        CoreError::Config(
            "config has no root directory (loaded via parse, not load) — cannot resolve agent paths"
                .to_owned(),
        )
    };
    let mut layers = Vec::new();
    if let Some(root) = cfg.root.as_deref() {
        layers.push(root.join("agents"));
    } else {
        return Err(need_root());
    }
    Ok(layers)
}

/// One layer's agent definitions by name; duplicate names in a layer are a
/// hard error (fail fast — silent shadowing within one directory hides a
/// pack bug). Reads agent DIRECTORIES (compat §5.1).
fn load_layer(
    dir: &Path,
    defaults: &AgentDefaults,
) -> Result<BTreeMap<String, AgentDef>, CoreError> {
    let mut defs = BTreeMap::new();
    if !dir.is_dir() {
        return Ok(defs); // a pack without agents/ contributes nothing
    }
    let entries = std::fs::read_dir(dir).map_err(|e| pack_err(dir, format!("cannot read: {e}")))?;
    let mut dirs: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| pack_err(dir, format!("cannot read entry: {e}")))?;
        let path = entry.path();
        if path.is_dir() {
            dirs.push(path);
        }
    }
    dirs.sort();
    for d in dirs {
        let (raw, _refusals) = parse_agent_dir(&d)?;
        let def = AgentDef {
            name: raw.name.clone(),
            model: defaults.model.clone(),
            tools: defaults.tools.clone(),
            permission_mode: defaults.permission_mode.clone(),
            isolation: Isolation::Worktree,
            stall_after: raw.stall_after.clone(),
            prompt: raw.prompt,
        };
        if let Some(previous) = defs.insert(def.name.clone(), def) {
            return Err(pack_err(
                dir,
                format!("two dirs define agent {:?} in one layer", previous.name),
            ));
        }
    }
    Ok(defs)
}

/// Resolve an agent by name across the configured layers, last wins
/// (spec §11; master plan Phase 8 pinned signature).
pub fn resolve_agent(cfg: &CampConfig, name: &str) -> Result<AgentDef, CoreError> {
    let layers = layers(cfg)?;
    let mut found: Option<AgentDef> = None;
    for dir in &layers {
        if let Some(def) = load_layer(dir, &cfg.agent_defaults)?.remove(name) {
            found = Some(def);
        }
    }
    found.ok_or_else(|| CoreError::UnknownAgent {
        name: name.to_owned(),
        searched: layers.iter().map(|p| p.display().to_string()).collect(),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn write_agent_dir(root: &Path, name: &str, agent_toml: Option<&str>, prompt_file: &str, prompt: &str) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        if let Some(t) = agent_toml {
            std::fs::write(dir.join("agent.toml"), t).unwrap();
        }
        std::fs::write(dir.join(prompt_file), prompt).unwrap();
    }

    #[test]
    fn agent_toml_tolerates_unknown_fallback_key() {
        let dir = tempfile::tempdir().unwrap();
        write_agent_dir(
            dir.path(),
            "architect",
            Some("description = \"BMAD architecture planner\"\nscope = \"rig\"\nfallback = true\n"),
            "prompt.template.md",
            "You are the architect. {{.Var}}",
        );
        let (agent, refusals) = parse_agent_dir(&dir.path().join("architect")).unwrap();
        assert_eq!(agent.name, "architect");
        assert_eq!(agent.scope.as_deref(), Some("rig"));
        assert!(refusals.is_empty(), "fallback is ignored, not refused");
    }

    #[test]
    fn prompt_precedence_prefers_template_md() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::write(a.join("prompt.md"), "plain").unwrap();
        std::fs::write(a.join("prompt.template.md"), "templated").unwrap();
        assert_eq!(parse_agent_dir(&a).unwrap().0.prompt, "templated");
    }

    #[test]
    fn identity_is_the_directory_name_not_a_field() {
        let dir = tempfile::tempdir().unwrap();
        write_agent_dir(dir.path(), "run-operator", Some("name = \"something-else\"\n"), "prompt.md", "operate");
        assert_eq!(parse_agent_dir(&dir.path().join("run-operator")).unwrap().0.name, "run-operator");
    }

    #[test]
    fn unsupported_keys_are_refused_and_named() {
        let dir = tempfile::tempdir().unwrap();
        write_agent_dir(
            dir.path(),
            "pooled",
            Some("work_dir = \"x\"\nmax_active_sessions = 3\npre_start = \"boot\"\n"),
            "prompt.md",
            "p",
        );
        let (_a, refusals) = parse_agent_dir(&dir.path().join("pooled")).unwrap();
        let keys: std::collections::BTreeSet<_> = refusals.iter().map(|r| r.key.as_str()).collect();
        assert!(keys.contains("work_dir") && keys.contains("max_active_sessions") && keys.contains("pre_start"), "{keys:?}");
        assert!(refusals.iter().all(|r| r.agent == "pooled"));
    }

    #[test]
    fn missing_prompt_is_a_hard_error() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::write(a.join("agent.toml"), "scope=\"rig\"\n").unwrap();
        assert!(parse_agent_dir(&a).unwrap_err().to_string().contains("prompt"));
    }

    #[test]
    fn stall_after_validates_via_parse_duration() {
        let dir = tempfile::tempdir().unwrap();
        write_agent_dir(dir.path(), "a", Some("stall_after = \"5m\"\n"), "prompt.md", "p");
        assert_eq!(parse_agent_dir(&dir.path().join("a")).unwrap().0.stall_after.as_deref(), Some("5m"));
        write_agent_dir(dir.path(), "b", Some("stall_after = \"banana\"\n"), "prompt.md", "p");
        assert!(parse_agent_dir(&dir.path().join("b")).unwrap_err().to_string().contains("stall_after"));
    }
}
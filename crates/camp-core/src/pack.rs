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
    /// §2.2 (cp-4): spawn this agent's workers with --include-partial-messages so
    /// a live `camp attach` sees token deltas. Default false -- autonomous-only
    /// agents never emit deltas. Parsed from `agent.toml`'s `partial_messages`.
    pub partial_messages: bool,
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
    /// `isolation = "none"` opt-out (spec §12 dispatch-lifecycle Q1): the
    /// agent runs on the rig's live tree. `None` → the DEFAULT (`Worktree`).
    pub isolation: Option<Isolation>,
    /// §2.2 (cp-4): the raw `partial_messages` bool from `agent.toml` (default
    /// false). Threaded to `AgentDef.partial_messages`, then to the spawn gate.
    pub partial_messages: bool,
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
            p.is_file().then_some(p)
        })
        .ok_or_else(|| {
            pack_err(
                dir,
                "no prompt file (expected prompt.template.md, prompt.md.tmpl, or prompt.md)",
            )
        })?;
    let prompt = std::fs::read_to_string(&prompt)
        .map_err(|e| pack_err(dir, format!("cannot read {}: {e}", prompt.display())))?;
    if prompt.trim().is_empty() {
        return Err(pack_err(
            dir,
            "empty prompt — an agent must say what it does",
        ));
    }

    let mut scope = None;
    let mut stall_after = None;
    let mut isolation = None;
    let mut partial_messages = false;
    let mut refusals = Vec::new();
    let agent_toml = dir.join("agent.toml");
    if agent_toml.is_file() {
        let text = std::fs::read_to_string(&agent_toml)
            .map_err(|e| pack_err(dir, format!("cannot read agent.toml: {e}")))?;
        let doc: toml::Value = toml::from_str(&text)
            .map_err(|e| pack_err(dir, format!("agent.toml is not valid TOML: {e}")))?;
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
                    "isolation" => {
                        let s = value.as_str().ok_or_else(|| {
                            pack_err(dir, "agent.toml key \"isolation\" must be a string")
                        })?;
                        isolation = Some(match s {
                            "worktree" => Isolation::Worktree,
                            "none" => Isolation::None,
                            other => {
                                return Err(pack_err(
                                    dir,
                                    format!(
                                        "agent.toml key \"isolation\" accepts only \"worktree\" or \
                                         \"none\", got {other:?}"
                                    ),
                                ));
                            }
                        });
                    }
                    "partial_messages" => {
                        partial_messages = value.as_bool().ok_or_else(|| {
                            pack_err(dir, "agent.toml key \"partial_messages\" must be a boolean")
                        })?;
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
            isolation,
            partial_messages,
        },
        refusals,
    ))
}

/// Build an `AgentDef` from operator `[agent_defaults]` + a parsed agent
/// directory (§5.2/§5.3). Model/permission_mode/tools come ONLY from
/// `defaults` — camp never inherits gc's unrestricted default. No resolvable
/// `tools` → no spawn (refused, naming the remedy). A pack that ships
/// `skills/` (`pack_ships_skills`) requires `Skill` in the allowlist, else
/// refused with both remedies (add `Skill`, or `skills = false` on the
/// import). `AgentDef` keeps its existing fields so `spawn.rs` is untouched.
pub fn resolve_agent_def(
    defaults: &AgentDefaults,
    raw: &RawAgent,
    qualified_name: &str,
    pack_ships_skills: bool,
) -> Result<AgentDef, CoreError> {
    let tools = defaults.tools.clone().ok_or_else(|| {
        CoreError::Pack(format!(
            "agent {qualified_name:?}: no tool allowlist resolves — set [agent_defaults].tools \
             in camp.toml (camp never inherits gc's unrestricted default)"
        ))
    })?;
    if pack_ships_skills && !tools.iter().any(|t| t == "Skill") {
        return Err(CoreError::Pack(format!(
            "agent {qualified_name:?}: the pack ships skills/ but {tools:?} lacks \"Skill\" — \
             add `Skill` to `[agent_defaults].tools`, or set `skills = false` on the import"
        )));
    }
    Ok(AgentDef {
        name: qualified_name.to_owned(),
        model: defaults.model.clone(),
        tools: Some(tools),
        permission_mode: defaults.permission_mode.clone(),
        isolation: raw.isolation.unwrap_or(Isolation::Worktree),
        stall_after: raw.stall_after.clone(),
        partial_messages: raw.partial_messages,
        prompt: raw.prompt.clone(),
    })
}

/// Resolve an agent by its qualified name (umbrella §7.1; master plan
/// Phase 8 pinned signature — unchanged so `dispatch.rs`/`patrol.rs`/
/// `sling.rs`/`spawn.rs` never need editing).
///
/// Split at the FIRST dot: the prefix is a binding in `cfg.imports` (else
/// fail-fast naming the binding + the `camp import add <source> --name
/// <binding>` remedy); the suffix is `<root>/imports/<binding>/agents/
/// <suffix>/` (missing → `UnknownAgent`). A no-dot name resolves a camp-local
/// agent at `<root>/agents/<name>/` (bare, disjoint from every binding).
/// `gstack.review-synthesizer` + `gc.review-synthesizer` coexist by
/// construction. `pack_ships_skills` is true when the import materialized a
/// `skills/` dir AND the import's `skills != Some(false)`.
/// An agent's directory name: the binding charset (`[A-Za-z0-9_-]+`). Agent
/// names arrive from PACK CONTENT — a formula's `route`/`assignee` — which is
/// untrusted input, and the name is joined straight onto a filesystem path. A
/// route of `gc.../../../../etc/some-agent` would otherwise walk out of the
/// materialization root and read a prompt from anywhere on disk. The charset
/// excludes `.`, `..`, and `/` by construction.
fn valid_agent_dirname(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

pub fn resolve_agent(cfg: &CampConfig, name: &str) -> Result<AgentDef, CoreError> {
    let root = cfg.root.as_deref().ok_or_else(|| {
        CoreError::Config(
            "config has no root directory (loaded via parse, not load) — cannot resolve agent paths"
                .to_owned(),
        )
    })?;
    // Validate BEFORE any path join: the suffix (or a bare name) becomes a
    // directory component, so a traversal must never reach the filesystem.
    let dirname = name.split_once('.').map_or(name, |(_, suffix)| suffix);
    if !valid_agent_dirname(dirname) {
        return Err(CoreError::UnknownAgent {
            name: name.to_owned(),
            searched: vec![format!(
                "{dirname:?} is not a legal agent name ([A-Za-z0-9_-]+)"
            )],
        });
    }
    match name.split_once('.') {
        Some((binding, suffix)) => {
            let decl = cfg.imports.get(binding).ok_or_else(|| {
                CoreError::Pack(format!(
                    "agent {name:?}: no binding {binding:?} in camp.toml — run \
                     `camp import add <source> --name {binding}`"
                ))
            })?;
            // The import's layer dir: IN PLACE for a local path, the derived
            // <root>/imports/<binding>/ for a git source (§5, D7). Agents come
            // from the DIRECT import only — a transitive pack contributes
            // content layers, never agents (§7.2), which is exactly why a
            // direct import overrides a transitive binding (§7.1, D8).
            let layer = decl.layer_dir(root, binding);
            let dir = layer.join("agents").join(suffix);
            if !dir.is_dir() {
                return Err(CoreError::UnknownAgent {
                    name: name.to_owned(),
                    searched: vec![dir.display().to_string()],
                });
            }
            let (raw, _refusals) = parse_agent_dir(&dir)?;
            // The §5.3 gate must reflect what ACTUALLY installs — the direct
            // layer AND every transitive one (#118). Deriving it from
            // `resolve_agent_skill_layers` keeps one source of truth: whatever
            // dispatch installs is exactly what the allowlist is checked against.
            let pack_ships_skills = !resolve_agent_skill_layers(cfg, name)?.is_empty();
            resolve_agent_def(&cfg.agent_defaults, &raw, name, pack_ships_skills)
        }
        None => {
            let dir = root.join("agents").join(name);
            if !dir.is_dir() {
                return Err(CoreError::UnknownAgent {
                    name: name.to_owned(),
                    searched: vec![dir.display().to_string()],
                });
            }
            let (raw, _refusals) = parse_agent_dir(&dir)?;
            // A camp-local agent brings no binding layer, but it still receives
            // the transitive content layers' skills — so it needs the same gate.
            let pack_ships_skills = !resolve_agent_skill_layers(cfg, name)?.is_empty();
            resolve_agent_def(&cfg.agent_defaults, &raw, name, pack_ships_skills)
        }
    }
}

/// The ORDERED pack-layer directories that WILL install skills into the
/// dispatched agent `name`'s worktree (issue #118, umbrella §5.3/§7.2).
///
/// Each returned path is a LAYER dir — the argument `import::skills::install_skills`
/// expects, not the `skills/` subdir (it appends `skills/` itself). Only layers
/// that actually ship a `skills/` dir are returned, so this is the ONE answer to
/// "will any skills install for this agent?" — `resolve_agent` derives the
/// `Skill`-allowlist gate from it, and dispatch installs exactly these. The
/// order is load-bearing: all TRANSITIVE content layers (§7.2 shared content)
/// come FIRST, the agent's DIRECT binding layer LAST, so a direct import wins a
/// skill-name collision (§7.1).
///
/// The direct binding layer is included only for a dotted `<binding>.<agent>`
/// name AND only when the import does not opt out (`skills != Some(false)`). A
/// no-dot camp-local agent contributes NO binding layer (a local agent is never
/// a pack), yet still receives every transitive content layer — so `skills =
/// false` on a direct import scopes to THAT import only, never to the shared
/// transitive layers other bindings also read (§7.2). The unknown-binding and
/// missing-root errors mirror `resolve_agent`'s shapes.
pub fn resolve_agent_skill_layers(cfg: &CampConfig, name: &str) -> Result<Vec<PathBuf>, CoreError> {
    let root = cfg.root.as_deref().ok_or_else(|| {
        CoreError::Config(
            "config has no root directory (loaded via parse, not load) — cannot resolve agent paths"
                .to_owned(),
        )
    })?;
    // Transitive content layers first (§7.2), already sorted by binding.
    let mut layers: Vec<PathBuf> = cfg
        .transitive_layers()?
        .into_iter()
        .map(|(_binding, dir)| dir)
        .collect();
    // Then the DIRECT binding layer for a dotted name — LAST, so it overrides a
    // transitive skill of the same name (§7.1). Honor the `skills = false`
    // opt-out; a no-dot local agent adds no binding layer.
    if let Some((binding, _suffix)) = name.split_once('.') {
        let decl = cfg.imports.get(binding).ok_or_else(|| {
            CoreError::Pack(format!(
                "agent {name:?}: no binding {binding:?} in camp.toml — run \
                 `camp import add <source> --name {binding}`"
            ))
        })?;
        if decl.skills != Some(false) {
            layers.push(decl.layer_dir(root, binding));
        }
    }
    // Only layers that REALLY ship skills/. Keeping this the sole definition of
    // "skills will install" is what keeps the §5.3 allowlist gate honest: a
    // transitive layer's skills used to install while `resolve_agent` computed
    // `pack_ships_skills` from the direct layer alone, so the gate stayed silent
    // and the worker got skill files it had no `Skill` tool to invoke.
    layers.retain(|layer| layer.join("skills").is_dir());
    Ok(layers)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn write_agent_dir(
        root: &Path,
        name: &str,
        agent_toml: Option<&str>,
        prompt_file: &str,
        prompt: &str,
    ) {
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
        write_agent_dir(
            dir.path(),
            "run-operator",
            Some("name = \"something-else\"\n"),
            "prompt.md",
            "operate",
        );
        assert_eq!(
            parse_agent_dir(&dir.path().join("run-operator"))
                .unwrap()
                .0
                .name,
            "run-operator"
        );
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
        assert!(
            keys.contains("work_dir")
                && keys.contains("max_active_sessions")
                && keys.contains("pre_start"),
            "{keys:?}"
        );
        assert!(refusals.iter().all(|r| r.agent == "pooled"));
    }

    #[test]
    fn missing_prompt_is_a_hard_error() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::write(a.join("agent.toml"), "scope=\"rig\"\n").unwrap();
        assert!(
            parse_agent_dir(&a)
                .unwrap_err()
                .to_string()
                .contains("prompt")
        );
    }

    fn defaults(tools: Option<Vec<&str>>) -> AgentDefaults {
        AgentDefaults {
            model: Some("sonnet".into()),
            permission_mode: Some("acceptEdits".into()),
            tools: tools.map(|v| v.iter().map(|s| s.to_string()).collect()),
        }
    }
    fn raw(name: &str) -> RawAgent {
        RawAgent {
            name: name.into(),
            prompt: "p".into(),
            scope: None,
            stall_after: None,
            isolation: None,
            partial_messages: false,
        }
    }
    #[test]
    fn agent_def_takes_model_permission_tools_from_operator_defaults() {
        let def = resolve_agent_def(
            &defaults(Some(vec!["Read", "Edit", "Bash"])),
            &raw("architect"),
            "bmad.architect",
            false,
        )
        .unwrap();
        assert_eq!(def.name, "bmad.architect");
        assert_eq!(def.model.as_deref(), Some("sonnet"));
        assert_eq!(def.permission_mode.as_deref(), Some("acceptEdits"));
        assert_eq!(def.tools.as_deref().unwrap(), ["Read", "Edit", "Bash"]);
    }
    #[test]
    fn agent_without_resolved_tools_is_refused() {
        let m = resolve_agent_def(&defaults(None), &raw("architect"), "bmad.architect", false)
            .unwrap_err()
            .to_string();
        assert!(m.contains("tools") && m.contains("agent_defaults"), "{m}");
    }
    #[test]
    fn skill_missing_from_allowlist_is_refused_with_remedies() {
        let m = resolve_agent_def(
            &defaults(Some(vec!["Read", "Edit"])),
            &raw("architect"),
            "bmad.architect",
            true,
        )
        .unwrap_err()
        .to_string();
        assert!(
            m.contains("Skill") && m.contains("skills = false") && m.contains("[agent_defaults]"),
            "{m}"
        );
    }
    #[test]
    fn skill_present_allows_a_skills_pack() {
        assert!(
            resolve_agent_def(
                &defaults(Some(vec!["Read", "Skill"])),
                &raw("architect"),
                "bmad.architect",
                true
            )
            .is_ok()
        );
    }

    fn camp_with_imports(kv: &[(&str, &str)]) -> (tempfile::TempDir, CampConfig) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut toml = String::from("[camp]\nname=\"t\"\n[agent_defaults]\ntools=[\"Read\"]\n");
        for (binding, _agent) in kv {
            toml.push_str(&format!("[imports.{binding}]\nsource=\"file:///unused\"\n"));
        }
        for (binding, agent) in kv {
            let a = root
                .join("imports")
                .join(binding)
                .join("agents")
                .join(agent);
            std::fs::create_dir_all(&a).unwrap();
            std::fs::write(a.join("prompt.md"), format!("I am {binding}.{agent}")).unwrap();
        }
        std::fs::write(root.join("camp.toml"), &toml).unwrap();
        let cfg = CampConfig::load(&root.join("camp.toml")).unwrap();
        (dir, cfg)
    }
    #[test]
    fn qualified_route_resolves_through_binding() {
        let (_d, cfg) = camp_with_imports(&[("gc", "run-operator")]);
        let def = resolve_agent(&cfg, "gc.run-operator").unwrap();
        assert_eq!(def.name, "gc.run-operator");
        assert!(def.prompt.contains("gc.run-operator"));
    }

    /// An agent name is joined onto a filesystem path, and it arrives from PACK
    /// CONTENT (a formula's `route`/`assignee`) — untrusted input. A traversal
    /// in the suffix must never reach the filesystem, or a crafted route reads a
    /// prompt from outside the materialization root and feeds it to a worker.
    #[test]
    fn a_qualified_agent_name_cannot_traverse_out_of_the_import_root() {
        let (dir, cfg) = camp_with_imports(&[("gc", "run-operator")]);
        // Plant a real agent dir OUTSIDE any import, reachable only by escaping.
        let outside = dir.path().join("outside/evil");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("prompt.md"), "PWNED").unwrap();

        for name in [
            "gc../../outside/evil",
            "gc./../outside/evil",
            "gc.../..",
            "gc./",
        ] {
            let err = resolve_agent(&cfg, name).unwrap_err();
            assert!(
                matches!(err, CoreError::UnknownAgent { .. }),
                "{name:?} must be refused as an agent name, got {err:?}"
            );
        }
        // The bare-name layer is guarded by the same charset.
        assert!(resolve_agent(&cfg, "../outside/evil").is_err());
    }
    #[test]
    fn route_to_unbound_binding_fails_naming_remedy() {
        let (_d, cfg) = camp_with_imports(&[("gc", "run-operator")]);
        let m = resolve_agent(&cfg, "bmad.architect")
            .unwrap_err()
            .to_string();
        assert!(
            m.contains("bmad") && m.contains("camp import add") && m.contains("--name bmad"),
            "{m}"
        );
    }
    #[test]
    fn same_name_across_bindings_coexists() {
        let (_d, cfg) = camp_with_imports(&[
            ("gstack", "review-synthesizer"),
            ("gc", "review-synthesizer"),
        ]);
        assert!(
            resolve_agent(&cfg, "gstack.review-synthesizer")
                .unwrap()
                .prompt
                .contains("gstack")
        );
        assert!(
            resolve_agent(&cfg, "gc.review-synthesizer")
                .unwrap()
                .prompt
                .contains("gc")
        );
    }
    #[test]
    fn bare_name_resolves_a_camp_local_agent() {
        let (_d, cfg) = camp_with_imports(&[]);
        let a = cfg.root.clone().unwrap().join("agents/dev");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::write(a.join("prompt.md"), "local dev").unwrap();
        assert_eq!(resolve_agent(&cfg, "dev").unwrap().name, "dev");
    }

    #[test]
    fn stall_after_validates_via_parse_duration() {
        let dir = tempfile::tempdir().unwrap();
        write_agent_dir(
            dir.path(),
            "a",
            Some("stall_after = \"5m\"\n"),
            "prompt.md",
            "p",
        );
        assert_eq!(
            parse_agent_dir(&dir.path().join("a"))
                .unwrap()
                .0
                .stall_after
                .as_deref(),
            Some("5m")
        );
        write_agent_dir(
            dir.path(),
            "b",
            Some("stall_after = \"banana\"\n"),
            "prompt.md",
            "p",
        );
        assert!(
            parse_agent_dir(&dir.path().join("b"))
                .unwrap_err()
                .to_string()
                .contains("stall_after")
        );
    }

    #[test]
    fn agent_toml_isolation_none_is_honored() {
        // spec §12 dispatch-lifecycle Q1: `isolation = "none"` is the explicit
        // opt-out — the agent runs on the rig's live tree. Default is Worktree.
        let dir = tempfile::tempdir().unwrap();
        write_agent_dir(
            dir.path(),
            "live",
            Some("isolation = \"none\"\n"),
            "prompt.md",
            "p",
        );
        let def = resolve_agent_def(
            &defaults(Some(vec!["Read"])),
            &parse_agent_dir(&dir.path().join("live")).unwrap().0,
            "gc.live",
            false,
        )
        .unwrap();
        assert_eq!(def.isolation, Isolation::None);

        write_agent_dir(
            dir.path(),
            "wt",
            Some("isolation = \"worktree\"\n"),
            "prompt.md",
            "p",
        );
        let def = resolve_agent_def(
            &defaults(Some(vec!["Read"])),
            &parse_agent_dir(&dir.path().join("wt")).unwrap().0,
            "gc.wt",
            false,
        )
        .unwrap();
        assert_eq!(def.isolation, Isolation::Worktree);

        // undeclared → the DEFAULT (Worktree)
        write_agent_dir(dir.path(), "dflt", None, "prompt.md", "p");
        let def = resolve_agent_def(
            &defaults(Some(vec!["Read"])),
            &parse_agent_dir(&dir.path().join("dflt")).unwrap().0,
            "gc.dflt",
            false,
        )
        .unwrap();
        assert_eq!(def.isolation, Isolation::Worktree);

        // unknown value → hard error naming the key
        write_agent_dir(
            dir.path(),
            "bad",
            Some("isolation = \"bubble\"\n"),
            "prompt.md",
            "p",
        );
        assert!(
            parse_agent_dir(&dir.path().join("bad"))
                .unwrap_err()
                .to_string()
                .contains("isolation")
        );
    }

    #[test]
    fn partial_messages_defaults_false_and_reads_from_agent_toml() {
        // §2.2 (cp-4): `partial_messages = true` in agent.toml resolves to true;
        // an agent without the key resolves to false (autonomous-only default).
        let dir = tempfile::tempdir().unwrap();
        write_agent_dir(
            dir.path(),
            "attachable",
            Some("partial_messages = true\n"),
            "prompt.md",
            "p",
        );
        let def = resolve_agent_def(
            &defaults(Some(vec!["Read"])),
            &parse_agent_dir(&dir.path().join("attachable")).unwrap().0,
            "gc.attachable",
            false,
        )
        .unwrap();
        assert!(def.partial_messages, "the key opts in");

        // undeclared → false (autonomous dispatch never emits token deltas)
        write_agent_dir(dir.path(), "dflt", None, "prompt.md", "p");
        let def = resolve_agent_def(
            &defaults(Some(vec!["Read"])),
            &parse_agent_dir(&dir.path().join("dflt")).unwrap().0,
            "gc.dflt",
            false,
        )
        .unwrap();
        assert!(!def.partial_messages, "default is off");

        // a non-boolean value is a hard error naming the key (fail fast)
        write_agent_dir(
            dir.path(),
            "bad",
            Some("partial_messages = \"yes\"\n"),
            "prompt.md",
            "p",
        );
        assert!(
            parse_agent_dir(&dir.path().join("bad"))
                .unwrap_err()
                .to_string()
                .contains("partial_messages")
        );
    }

    // ---- issue #118: resolve_agent_skill_layers ---------------------------

    /// A camp whose `bmad` import ships an `architect` agent AND a `skills/`
    /// dir (a git-source import, so its layer dir is `imports/bmad`). Returns
    /// (tempdir, loaded cfg).
    fn camp_with_skill_import(skills_false: bool) -> (tempfile::TempDir, CampConfig) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let opt_out = if skills_false { "skills=false\n" } else { "" };
        let toml = format!(
            "[camp]\nname=\"t\"\n[agent_defaults]\ntools=[\"Read\",\"Skill\"]\n\
             [imports.bmad]\nsource=\"https://example.com/bmad\"\n{opt_out}"
        );
        let a = root.join("imports/bmad/agents/architect");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::write(a.join("prompt.md"), "architect").unwrap();
        let s = root.join("imports/bmad/skills/bmad-arch");
        std::fs::create_dir_all(&s).unwrap();
        std::fs::write(s.join("SKILL.md"), "# skill").unwrap();
        std::fs::write(root.join("camp.toml"), &toml).unwrap();
        let cfg = CampConfig::load(&root.join("camp.toml")).unwrap();
        (dir, cfg)
    }

    #[test]
    fn resolve_agent_skill_layers_returns_the_direct_binding_layer() {
        let (dir, cfg) = camp_with_skill_import(false);
        let layers = resolve_agent_skill_layers(&cfg, "bmad.architect").unwrap();
        let direct = dir.path().join("imports/bmad");
        assert!(
            layers.contains(&direct),
            "the direct binding layer must be a skill-install layer: {layers:?}"
        );
    }

    #[test]
    fn resolve_agent_skill_layers_honors_the_skills_false_opt_out() {
        let (dir, cfg) = camp_with_skill_import(true);
        let layers = resolve_agent_skill_layers(&cfg, "bmad.architect").unwrap();
        let direct = dir.path().join("imports/bmad");
        assert!(
            !layers.contains(&direct),
            "skills = false must EXCLUDE the direct binding layer: {layers:?}"
        );
    }

    #[test]
    fn resolve_agent_skill_layers_puts_transitive_layers_before_the_direct_layer() {
        let (dir, cfg) = camp_with_skill_import(false);
        let root = dir.path();
        // Plant a transitive content layer (§7.2) shipping its own skills/ —
        // transitive_layers() reads the sentinel dir live off disk.
        let t = root.join("imports/.transitive/gc/skills/gc-skill");
        std::fs::create_dir_all(&t).unwrap();
        std::fs::write(t.join("SKILL.md"), "# gc").unwrap();

        let layers = resolve_agent_skill_layers(&cfg, "bmad.architect").unwrap();
        let transitive = root.join("imports/.transitive/gc");
        let direct = root.join("imports/bmad");
        let ti = layers.iter().position(|p| p == &transitive);
        let di = layers.iter().position(|p| p == &direct);
        assert!(
            ti.is_some(),
            "transitive layer must be included: {layers:?}"
        );
        assert!(di.is_some(), "direct layer must be included: {layers:?}");
        assert!(
            ti < di,
            "transitive layers install BEFORE the direct layer: {layers:?}"
        );
    }

    /// #118 review finding 2: the §5.3 allowlist gate must fire when a
    /// TRANSITIVE layer's skills will install, not just the direct layer's.
    /// Otherwise an agent whose allowlist lacks `Skill` resolves fine, then
    /// receives transitive skill files it can never invoke — silently
    /// under-provisioned, which is the exact failure the gate exists to make
    /// loud. Direct layer ships NO skills/ here; the transitive layer does.
    #[test]
    fn transitive_skills_still_require_the_skill_allowlist() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("camp.toml"),
            "[camp]\nname=\"t\"\n[agent_defaults]\ntools=[\"Read\"]\n\
             [imports.bmad]\nsource=\"https://example.com/bmad\"\n",
        )
        .unwrap();
        // direct layer: an agent, but NO skills/ of its own
        let a = root.join("imports/bmad/agents/architect");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::write(a.join("prompt.md"), "architect").unwrap();
        // a TRANSITIVE content layer that DOES ship skills/
        let t = root.join("imports/.transitive/gc/skills/gc-skill");
        std::fs::create_dir_all(&t).unwrap();
        std::fs::write(t.join("SKILL.md"), "# gc").unwrap();
        let cfg = CampConfig::load(&root.join("camp.toml")).unwrap();

        // those transitive skills WILL install...
        let layers = resolve_agent_skill_layers(&cfg, "bmad.architect").unwrap();
        assert!(
            layers.contains(&root.join("imports/.transitive/gc")),
            "the transitive layer's skills install: {layers:?}"
        );
        // ...so resolving the agent must REFUSE the missing `Skill` allowlist
        let err = resolve_agent(&cfg, "bmad.architect")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("Skill"),
            "transitive skills must trip the allowlist gate: {err}"
        );
    }

    #[test]
    fn resolve_agent_skill_layers_fails_fast_on_an_unbound_binding() {
        let (_d, cfg) = camp_with_skill_import(false);
        let m = resolve_agent_skill_layers(&cfg, "nope.architect")
            .unwrap_err()
            .to_string();
        assert!(m.contains("nope") && m.contains("camp import add"), "{m}");
    }
}

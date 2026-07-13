//! Packs (spec §11): agent definitions are Claude Code agent files —
//! YAML frontmatter + prompt body — read verbatim (zero invented formats).
//! Resolution layers packs from camp.toml in order, later wins, with the
//! camp-local agents/ directory highest (Phase 8 plan decisions A and R).
//! Unknown frontmatter keys are tolerated (Claude Code owns that format
//! and grows it); the keys camp reads are type-checked strictly. Format
//! facts verified 2026-07-07 against code.claude.com/docs/en/sub-agents
//! and 12 installed agent files (plan Task 4 provenance).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use yaml_rust2::{Yaml, YamlLoader};

use crate::config::CampConfig;
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

fn pack_err(path: &Path, reason: impl std::fmt::Display) -> CoreError {
    CoreError::Pack(format!("{}: {reason}", path.display()))
}

/// Parse one Claude Code agent definition file.
pub fn parse_agent_file(path: &Path) -> Result<AgentDef, CoreError> {
    let text =
        std::fs::read_to_string(path).map_err(|e| pack_err(path, format!("cannot read: {e}")))?;
    let rest = text.strip_prefix("---\n").ok_or_else(|| {
        pack_err(
            path,
            "missing frontmatter (expected a `---` fence on line 1)",
        )
    })?;
    let (front, body) = rest
        .split_once("\n---\n")
        .or_else(|| rest.split_once("\n---\r\n"))
        .ok_or_else(|| pack_err(path, "unterminated frontmatter (no closing `---` fence)"))?;
    let docs = YamlLoader::load_from_str(front)
        .map_err(|e| pack_err(path, format!("frontmatter is not valid YAML: {e}")))?;
    let doc = docs.first().cloned().unwrap_or(Yaml::Null);

    let get_str = |key: &str| -> Result<Option<String>, CoreError> {
        match &doc[key] {
            Yaml::BadValue | Yaml::Null => Ok(None),
            Yaml::String(s) => Ok(Some(s.clone())),
            other => Err(pack_err(
                path,
                format!("frontmatter key {key:?} must be a string, got {other:?}"),
            )),
        }
    };

    // Identity comes only from the name field (sub-agents docs) — required.
    let name = get_str("name")?
        .ok_or_else(|| pack_err(path, "missing required frontmatter key \"name\""))?;

    let tools = match &doc["tools"] {
        Yaml::BadValue | Yaml::Null => None,
        Yaml::String(s) => Some(
            s.split(',')
                .map(|t| t.trim().to_owned())
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>(),
        ),
        Yaml::Array(items) => {
            let mut tools = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    Yaml::String(s) => tools.push(s.trim().to_owned()),
                    other => {
                        return Err(pack_err(
                            path,
                            format!("frontmatter key \"tools\" list holds a non-string: {other:?}"),
                        ));
                    }
                }
            }
            Some(tools)
        }
        other => {
            return Err(pack_err(
                path,
                format!("frontmatter key \"tools\" must be a string or list, got {other:?}"),
            ));
        }
    };

    let isolation = match get_str("isolation")?.as_deref() {
        None => Isolation::default(),
        Some("worktree") => Isolation::Worktree,
        // The explicit opt-out (spec §12, dispatch-lifecycle Q1): the
        // agent intentionally runs on the rig's live tree; dispatch makes
        // that loud (`dispatch.live_tree`).
        Some("none") => Isolation::None,
        Some(other) => {
            return Err(pack_err(
                path,
                format!(
                    "frontmatter key \"isolation\" accepts only \"worktree\" or \"none\", got {other:?}"
                ),
            ));
        }
    };

    let stall_after = get_str("stall_after")?;
    if let Some(s) = &stall_after {
        crate::patrol::parse_duration(s)
            .map_err(|e| pack_err(path, format!("frontmatter key \"stall_after\": {e}")))?;
    }

    let prompt = body.trim().to_owned();
    if prompt.is_empty() {
        return Err(pack_err(
            path,
            "empty prompt body — an agent definition must say what the agent does",
        ));
    }

    Ok(AgentDef {
        name,
        model: get_str("model")?,
        tools,
        permission_mode: get_str("permissionMode")?,
        isolation,
        stall_after,
        prompt,
    })
}

/// The agents/ layers to search, lowest to highest (plan decision R).
/// Phase 1 interim: the `packs` field is gone (compat §7 — packs now import
/// under `<root>/imports/<binding>/`); the binding-qualified rewrite lands in
/// Task 12. Until then the camp-local `<root>/agents/` layer is the only one.
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
/// pack bug).
fn load_layer(dir: &Path) -> Result<BTreeMap<String, AgentDef>, CoreError> {
    let mut defs = BTreeMap::new();
    if !dir.is_dir() {
        return Ok(defs); // a pack without agents/ contributes nothing
    }
    let entries = std::fs::read_dir(dir).map_err(|e| pack_err(dir, format!("cannot read: {e}")))?;
    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| pack_err(dir, format!("cannot read entry: {e}")))?;
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "md") {
            paths.push(path);
        }
    }
    paths.sort();
    for path in paths {
        let def = parse_agent_file(&path)?;
        if let Some(previous) = defs.insert(def.name.clone(), def) {
            return Err(pack_err(
                dir,
                format!("two files define agent {:?} in one layer", previous.name),
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
        if let Some(def) = load_layer(dir)?.remove(name) {
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
    use crate::config::CampConfig;
    use std::path::Path;

    fn write_agent(dir: &Path, file: &str, content: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(file), content).unwrap();
    }

    const DEV: &str = "---\nname: dev\ndescription: implements changes\ntools: Read, Edit, Bash\nmodel: sonnet\npermissionMode: acceptEdits\n---\nImplement the change with TDD.\n";

    #[test]
    fn parses_a_claude_code_agent_file() {
        let dir = tempfile::tempdir().unwrap();
        write_agent(dir.path(), "dev.md", DEV);
        let def = parse_agent_file(&dir.path().join("dev.md")).unwrap();
        assert_eq!(def.name, "dev");
        assert_eq!(def.model.as_deref(), Some("sonnet"));
        assert_eq!(
            def.tools,
            Some(vec![
                "Read".to_owned(),
                "Edit".to_owned(),
                "Bash".to_owned()
            ])
        );
        assert_eq!(def.permission_mode.as_deref(), Some("acceptEdits"));
        // Phase 2 (dispatch-lifecycle Q1): no isolation key = the DEFAULT,
        // which is worktree.
        assert_eq!(def.isolation, Isolation::Worktree);
        assert_eq!(def.prompt, "Implement the change with TDD.");
    }

    #[test]
    fn stall_after_frontmatter_parses_and_validates() {
        let dir = tempfile::tempdir().unwrap();
        write_agent(
            dir.path(),
            "s.md",
            "---\nname: dev\nstall_after: 5m\n---\nWork.\n",
        );
        let def = parse_agent_file(&dir.path().join("s.md")).unwrap();
        assert_eq!(def.stall_after.as_deref(), Some("5m"));

        write_agent(dir.path(), "none.md", "---\nname: dev\n---\nWork.\n");
        let def = parse_agent_file(&dir.path().join("none.md")).unwrap();
        assert_eq!(def.stall_after, None);

        write_agent(
            dir.path(),
            "bad.md",
            "---\nname: dev\nstall_after: banana\n---\nWork.\n",
        );
        let err = parse_agent_file(&dir.path().join("bad.md")).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("stall_after") && msg.contains("bad.md"),
            "error must name the key and the file: {msg}"
        );
    }

    #[test]
    fn tools_accepts_a_yaml_list_and_isolation_worktree_parses() {
        let dir = tempfile::tempdir().unwrap();
        write_agent(
            dir.path(),
            "iso.md",
            "---\nname: iso\ntools:\n  - Read\n  - Bash\nisolation: worktree\n---\nWork isolated.\n",
        );
        let def = parse_agent_file(&dir.path().join("iso.md")).unwrap();
        assert_eq!(def.tools, Some(vec!["Read".to_owned(), "Bash".to_owned()]));
        assert_eq!(def.isolation, Isolation::Worktree);
    }

    #[test]
    fn isolation_none_is_an_accepted_explicit_opt_out() {
        let dir = tempfile::tempdir().unwrap();
        write_agent(
            dir.path(),
            "live.md",
            "---\nname: live\nisolation: none\n---\nWork on the live tree.\n",
        );
        let def = parse_agent_file(&dir.path().join("live.md")).unwrap();
        assert_eq!(def.isolation, Isolation::None);
    }

    #[test]
    fn isolation_defaults_to_worktree_when_undeclared() {
        let dir = tempfile::tempdir().unwrap();
        write_agent(dir.path(), "d.md", "---\nname: d\n---\nWork.\n");
        let def = parse_agent_file(&dir.path().join("d.md")).unwrap();
        assert_eq!(def.isolation, Isolation::Worktree);
    }

    #[test]
    fn unknown_keys_are_tolerated_but_a_missing_name_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        // unknown/unread keys (description, color, maxTurns…) are Claude
        // Code's business — tolerated (decision A)
        write_agent(
            dir.path(),
            "quiet.md",
            "---\nname: quiet\ndescription: d\ncolor: cyan\nmaxTurns: 3\n---\nPrompt.\n",
        );
        let def = parse_agent_file(&dir.path().join("quiet.md")).unwrap();
        assert_eq!(def.name, "quiet");
        assert_eq!(def.prompt, "Prompt.");

        // name is required: identity comes only from the name field
        // (sub-agents docs), so a nameless file is a hard error
        write_agent(dir.path(), "anon.md", "---\ndescription: d\n---\nPrompt.\n");
        let err = parse_agent_file(&dir.path().join("anon.md")).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("name") && msg.contains("anon.md"),
            "error must name the missing key and the file: {msg}"
        );
    }

    #[test]
    fn malformed_files_fail_naming_the_file_and_problem() {
        let dir = tempfile::tempdir().unwrap();
        for (file, content, needle) in [
            ("nofm.md", "just a prompt\n", "frontmatter"),
            (
                "badiso.md",
                "---\nname: x\nisolation: bubble\n---\nP\n",
                "isolation",
            ),
            ("badtools.md", "---\nname: x\ntools: 7\n---\nP\n", "tools"),
            ("empty.md", "---\nname: x\n---\n\n", "prompt"),
        ] {
            write_agent(dir.path(), file, content);
            let err = parse_agent_file(&dir.path().join(file)).unwrap_err();
            let msg = err.to_string();
            assert!(
                msg.contains(needle) && msg.contains(file),
                "{file}: error {msg:?} must name {needle:?} and the file"
            );
        }
    }

    #[test]
    fn duplicate_agent_names_in_one_layer_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_agent(&root.join("agents"), "a.md", "---\nname: dev\n---\nOne.\n");
        write_agent(&root.join("agents"), "b.md", "---\nname: dev\n---\nTwo.\n");
        std::fs::write(root.join("camp.toml"), "[camp]\nname = \"t\"\n").unwrap();
        let cfg = CampConfig::load(&root.join("camp.toml")).unwrap();
        let err = resolve_agent(&cfg, "dev").unwrap_err();
        assert!(err.to_string().contains("dev"), "got {err}");
    }
}

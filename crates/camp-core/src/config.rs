//! camp.toml: the human-readable config that names the camp and its rigs
//! (spec §7.1, §12). Parsing is fail-fast — unknown keys are rejected
//! (`deny_unknown_fields`) so a typo never silently becomes dead config.
//! `camp.toml` is the source of truth for rigs; `rig.added` events are the
//! audit trail (spec §13.4), not a competing store.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::CoreError;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampConfig {
    pub camp: CampSection,
    #[serde(default)]
    pub rigs: Vec<RigConfig>,
    /// `[[order]]` tables (spec §9); compiled by `orders::parse::compile_orders`.
    #[serde(default, rename = "order", skip_serializing_if = "Vec::is_empty")]
    pub orders: Vec<crate::orders::parse::OrderConfig>,
    /// Pack directories (spec §11). Relative paths resolve against `root`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub packs: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "DispatchConfig::is_default")]
    pub dispatch: DispatchConfig,
    /// The directory containing camp.toml — set by `load`, never
    /// serialized. Needed to resolve relative pack paths and the local
    /// agents/ layer while keeping the master plan's pinned
    /// `resolve_agent(cfg, name)` signature (Phase 8 plan decision Q).
    #[serde(skip)]
    pub root: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DispatchConfig {
    /// Concurrency cap (spec §8.3); master plan Phase 8 default.
    #[serde(default = "default_max_workers")]
    pub max_workers: usize,
    /// Worker executable. Tests point this at fake-agent.sh — visible
    /// config, not a fallback (master plan Phase 8).
    #[serde(default = "default_command")]
    pub command: PathBuf,
    /// Camp-wide sling routing default (spec §8.1); rigs may override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_agent: Option<String>,
}

fn default_max_workers() -> usize {
    10
}

fn default_command() -> PathBuf {
    PathBuf::from("claude")
}

impl Default for DispatchConfig {
    fn default() -> Self {
        DispatchConfig {
            max_workers: default_max_workers(),
            command: default_command(),
            default_agent: None,
        }
    }
}

impl DispatchConfig {
    fn is_default(&self) -> bool {
        *self == DispatchConfig::default()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampSection {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RigConfig {
    pub name: String,
    pub path: PathBuf,
    pub prefix: String,
    /// Per-rig sling routing override (spec §8.1 "the pack's default
    /// worker for the current rig").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_agent: Option<String>,
}

impl CampConfig {
    /// Parse a camp.toml file. Missing file, bad TOML, and unknown keys are
    /// all hard errors.
    pub fn load(path: &Path) -> Result<CampConfig, CoreError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| CoreError::Config(format!("cannot read {}: {e}", path.display())))?;
        let mut cfg = CampConfig::parse(&text)?;
        cfg.root = path.parent().map(Path::to_path_buf);
        Ok(cfg)
    }

    pub fn parse(text: &str) -> Result<CampConfig, CoreError> {
        let cfg: CampConfig = toml::from_str(text).map_err(|e| CoreError::Config(e.to_string()))?;
        if cfg.dispatch.max_workers == 0 {
            // A typo'd cap must not silently disable dispatch (PR #14
            // review finding 5).
            return Err(CoreError::Config(
                "[dispatch] max_workers must be at least 1 (0 would disable dispatch entirely)"
                    .to_owned(),
            ));
        }
        Ok(cfg)
    }

    /// The rig with this name, or `UnknownRig`.
    pub fn rig(&self, name: &str) -> Result<&RigConfig, CoreError> {
        self.rigs
            .iter()
            .find(|r| r.name == name)
            .ok_or_else(|| CoreError::UnknownRig(name.to_owned()))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn parses_camp_and_rigs() {
        let cfg = CampConfig::parse(
            r#"
# a comment
[camp]
name = "dev"

[[rigs]]
name = "gascity"
path = "/code/gascity"
prefix = "gc"
"#,
        )
        .unwrap();
        assert_eq!(cfg.camp.name, "dev");
        assert_eq!(cfg.rigs.len(), 1);
        assert_eq!(cfg.rig("gascity").unwrap().prefix, "gc");
    }

    #[test]
    fn rigs_default_to_empty() {
        let cfg = CampConfig::parse("[camp]\nname = \"dev\"\n").unwrap();
        assert!(cfg.rigs.is_empty());
    }

    #[test]
    fn unknown_top_level_key_is_rejected() {
        let err = CampConfig::parse("[camp]\nname = \"dev\"\nbogus = 1\n").unwrap_err();
        assert!(matches!(err, CoreError::Config(_)), "got {err:?}");
    }

    #[test]
    fn unknown_rig_key_is_rejected() {
        let err = CampConfig::parse(
            "[camp]\nname=\"d\"\n[[rigs]]\nname=\"r\"\npath=\"/p\"\nprefix=\"r\"\nzzz=1\n",
        )
        .unwrap_err();
        assert!(matches!(err, CoreError::Config(_)), "got {err:?}");
    }

    #[test]
    fn missing_rig_is_unknown_rig() {
        let cfg = CampConfig::parse("[camp]\nname=\"d\"\n").unwrap();
        assert!(matches!(cfg.rig("nope"), Err(CoreError::UnknownRig(n)) if n == "nope"));
    }

    #[test]
    fn round_trips_through_toml() {
        let cfg = CampConfig {
            camp: CampSection { name: "dev".into() },
            rigs: vec![RigConfig {
                name: "gascity".into(),
                path: "/code/gascity".into(),
                prefix: "gc".into(),
                default_agent: None,
            }],
            orders: vec![],
            packs: Vec::new(),
            dispatch: DispatchConfig::default(),
            root: None,
        };
        let text = toml::to_string(&cfg).unwrap();
        assert_eq!(CampConfig::parse(&text).unwrap(), cfg);
    }

    // ---- Phase 8: [dispatch], packs, per-rig default_agent ---------------

    #[test]
    fn dispatch_and_packs_parse_with_defaults() {
        let cfg = CampConfig::parse(
            r#"
# top-level keys precede any [table] header (TOML), so packs comes first
packs = ["packs/starter", "/abs/otherpack"]

[camp]
name = "dev"

[[rigs]]
name = "gascity"
path = "/code/gascity"
prefix = "gc"
default_agent = "rigger"

[dispatch]
max_workers = 3
command = "tests/fake-agent.sh"
default_agent = "dev"
"#,
        )
        .unwrap();
        assert_eq!(
            cfg.packs,
            vec![
                PathBuf::from("packs/starter"),
                PathBuf::from("/abs/otherpack")
            ]
        );
        assert_eq!(cfg.dispatch.max_workers, 3);
        assert_eq!(cfg.dispatch.command, PathBuf::from("tests/fake-agent.sh"));
        assert_eq!(cfg.dispatch.default_agent.as_deref(), Some("dev"));
        assert_eq!(
            cfg.rig("gascity").unwrap().default_agent.as_deref(),
            Some("rigger")
        );
    }

    #[test]
    fn dispatch_section_is_optional_with_spec_defaults() {
        let cfg = CampConfig::parse("[camp]\nname = \"dev\"\n").unwrap();
        assert!(cfg.packs.is_empty());
        assert_eq!(cfg.dispatch.max_workers, 10);
        assert_eq!(cfg.dispatch.command, PathBuf::from("claude"));
        assert!(cfg.dispatch.default_agent.is_none());
        assert!(cfg.root.is_none(), "parse() has no file, so no root");
    }

    #[test]
    fn zero_max_workers_is_rejected_at_parse() {
        // A typo'd cap must not silently disable dispatch (PR #14 review
        // finding 5): fail fast, naming the field.
        let err =
            CampConfig::parse("[camp]\nname=\"d\"\n[dispatch]\nmax_workers = 0\n").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("max_workers"),
            "error must name the field: {msg}"
        );
    }

    #[test]
    fn unknown_dispatch_key_is_rejected() {
        let err = CampConfig::parse("[camp]\nname=\"d\"\n[dispatch]\nbogus = 1\n").unwrap_err();
        assert!(matches!(err, CoreError::Config(_)), "got {err:?}");
    }

    #[test]
    fn defaults_do_not_pollute_serialization() {
        // rig add appends TOML text rather than re-serializing, but the
        // config type must still round-trip cleanly without inventing
        // [dispatch]/packs blocks the user never wrote.
        let cfg = CampConfig::parse("[camp]\nname = \"dev\"\n").unwrap();
        let text = toml::to_string(&cfg).unwrap();
        assert!(!text.contains("dispatch"), "text was: {text}");
        assert!(!text.contains("packs"), "text was: {text}");
    }

    #[test]
    fn load_records_the_camp_root() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("camp.toml");
        std::fs::write(&path, "[camp]\nname = \"dev\"\n").unwrap();
        let cfg = CampConfig::load(&path).unwrap();
        assert_eq!(cfg.root.as_deref(), Some(dir.path()));
    }
}

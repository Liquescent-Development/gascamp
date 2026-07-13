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
    /// `[imports.<binding>]` (compat §7): the binding namespace. Each
    /// import materializes under `<root>/imports/<binding>/` and qualifies
    /// its agents/formulas/orders as `<binding>.<name>`.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub imports: std::collections::BTreeMap<String, ImportDecl>,
    /// `[orders] enabled = [...]` (compat §14): the money invariant — an
    /// imported order is INERT until this list names it. Distinct from the
    /// `[[order]]` array above (`rename = "order"`).
    #[serde(
        default,
        rename = "orders",
        skip_serializing_if = "OrdersSection::is_default"
    )]
    pub orders_section: OrdersSection,
    /// `[agent_defaults]` (compat §5.2): model/permission_mode/tools come
    /// ONLY from the operator, never from a pack — camp never inherits gc's
    /// unrestricted default.
    #[serde(default, skip_serializing_if = "AgentDefaults::is_default")]
    pub agent_defaults: AgentDefaults,
    #[serde(default, skip_serializing_if = "DispatchConfig::is_default")]
    pub dispatch: DispatchConfig,
    /// `[patrol]` (spec §10): stall threshold, restart budget, release
    /// grace. Validated at parse; typed via `patrol::PatrolConfig`.
    #[serde(default, skip_serializing_if = "PatrolSection::is_default")]
    pub patrol: PatrolSection,
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
    /// Bound on any subprocess campd runs inline on its single-threaded
    /// event loop — git worktree ops, adoption probes (issue #55). A hung
    /// one is killed at this deadline and surfaces as an error instead of
    /// wedging the daemon. A jiff-friendly duration string, strictly
    /// positive; a bound on the loop's worst-case stall, not a wakeup
    /// (invariant 1).
    #[serde(default = "default_exec_timeout")]
    pub exec_timeout: String,
}

fn default_max_workers() -> usize {
    10
}

fn default_command() -> PathBuf {
    PathBuf::from("claude")
}

fn default_exec_timeout() -> String {
    "60s".to_owned()
}

impl Default for DispatchConfig {
    fn default() -> Self {
        DispatchConfig {
            max_workers: default_max_workers(),
            command: default_command(),
            default_agent: None,
            exec_timeout: default_exec_timeout(),
        }
    }
}

impl DispatchConfig {
    fn is_default(&self) -> bool {
        *self == DispatchConfig::default()
    }

    /// `exec_timeout` resolved to a std Duration for deadline arithmetic.
    /// Validated at parse, so an Err here means the config was built by
    /// hand — still surfaced, never defaulted (invariant 5).
    pub fn exec_timeout(&self) -> Result<std::time::Duration, CoreError> {
        let d = crate::patrol::parse_duration(&self.exec_timeout)
            .map_err(|e| CoreError::Config(format!("[dispatch] exec_timeout: {e}")))?;
        std::time::Duration::try_from(d).map_err(|e| {
            CoreError::Config(format!(
                "[dispatch] exec_timeout {:?}: {e}",
                self.exec_timeout
            ))
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampSection {
    pub name: String,
}

/// `[patrol]` as written in camp.toml (spec §10). Durations are jiff
/// friendly strings; `patrol::PatrolConfig::from_section` resolves and
/// validates them (strictly positive).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatrolSection {
    /// Stall threshold: silence longer than this fires `agent.stalled`.
    /// Agent frontmatter `stall_after` overrides per agent.
    #[serde(default = "default_stall_after")]
    pub stall_after: String,
    /// Patrol restarts per bead per campd lifetime before the ladder
    /// exhausts (emit-and-stop).
    #[serde(default = "default_restart_budget")]
    pub restart_budget: u32,
    /// How long a released stream worker (bead closed, stdin dropped) may
    /// linger before campd terminates it (probe P3: idle stream workers do
    /// not exit on EOF).
    #[serde(default = "default_release_grace")]
    pub release_grace: String,
}

fn default_stall_after() -> String {
    "10m".to_owned()
}

fn default_restart_budget() -> u32 {
    2
}

fn default_release_grace() -> String {
    "30s".to_owned()
}

impl Default for PatrolSection {
    fn default() -> Self {
        PatrolSection {
            stall_after: default_stall_after(),
            restart_budget: default_restart_budget(),
            release_grace: default_release_grace(),
        }
    }
}

impl PatrolSection {
    fn is_default(&self) -> bool {
        *self == PatrolSection::default()
    }
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

/// One `[imports.<binding>]` declaration (compat §7). A binding qualifies
/// every agent/formula/order the import materializes as `<binding>.<name>`.
/// `trust_exec` defaults false (§13 default-deny); `skills = false` is the
/// §5.3 opt-out (a pack that ships `skills/` but should not install them).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportDecl {
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subpath: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub trust_exec: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<bool>,
}

/// The subdirectory of `<root>/imports/` that holds TRANSITIVE (pack-level,
/// §7.2) materializations. A leading `.` makes it unspellable as a binding
/// (`[A-Za-z0-9_-]+`), so a pack can never shadow it — and it keeps the
/// transitive content layers DISJOINT from the direct bindings' dirs, which
/// is what lets a direct import override a transitive one of the same name
/// (§7.1) without clobbering the transitive formula layers (D8).
pub const TRANSITIVE_DIR: &str = ".transitive";

/// Where a TRANSITIVE import's content layer is materialized (§7.2). Keyed by
/// its binding, under the transitive sentinel dir. Transitive packs contribute
/// content (formulas, fragments, skills, assets) by BARE name only — never
/// agents (a transitive `agents/` dir is refused at import).
pub fn transitive_layer_dir(root: &Path, binding: &str) -> PathBuf {
    root.join("imports").join(TRANSITIVE_DIR).join(binding)
}

impl ImportDecl {
    /// Is this import's source a LOCAL filesystem path? Delegates to the ONE
    /// definition of "local" (`import::source::is_local_source`), so a
    /// declared import and a CLI source can never be classified differently.
    pub fn is_local(&self) -> bool {
        crate::import::source::is_local_source(&self.source)
    }

    /// The directory this import's content is READ from (component §5).
    ///
    /// - **Local path → layered IN PLACE.** The source resolves relative to
    ///   `camp.toml`'s root and is read where the operator keeps it: no fetch,
    ///   no copy under `imports/`, no lock entry (§5's layout diagram). This
    ///   is the single seam that makes read-in-place true for EVERY resolver
    ///   (agents, formulas, orders, skills, exec inventory) at once.
    /// - **Git-backed → the DERIVED materialization** `<root>/imports/<binding>/`.
    ///   Never stored — storing it made a path read from a tracked file into a
    ///   write-anywhere primitive (§5).
    pub fn layer_dir(&self, root: &Path, binding: &str) -> PathBuf {
        if self.is_local() {
            let base = root.join(&self.source);
            return match &self.subpath {
                Some(s) => base.join(s),
                None => base,
            };
        }
        root.join("imports").join(binding)
    }
}

impl CampConfig {
    /// The DIRECT import layers as `(binding, dir)`, declaration-driven and
    /// sorted by binding (§7.1: the binding IS the namespace). Driven by
    /// `camp.toml`, NOT by listing `imports/` — a local import has no dir
    /// there, and the transitive sentinel must never be mistaken for a
    /// binding. Returns `None`-free pairs only when the config has a root.
    pub fn import_layers(&self) -> Vec<(String, PathBuf)> {
        let Some(root) = self.root.as_deref() else {
            return Vec::new();
        };
        self.imports
            .iter()
            .map(|(binding, decl)| (binding.clone(), decl.layer_dir(root, binding)))
            .collect()
    }

    /// The TRANSITIVE content layers as `(binding, dir)` (§7.2), sorted by
    /// binding. These contribute formulas/fragments/skills/assets by BARE
    /// name and sit BELOW the direct imports — so a direct import of the same
    /// binding overrides the transitive one's agents while its content layers
    /// survive (D8).
    pub fn transitive_layers(&self) -> Vec<(String, PathBuf)> {
        let Some(root) = self.root.as_deref() else {
            return Vec::new();
        };
        let dir = root.join("imports").join(TRANSITIVE_DIR);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut layers: Vec<(String, PathBuf)> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .filter_map(|p| {
                let binding = p.file_name()?.to_str()?.to_owned();
                Some((binding, p))
            })
            .collect();
        layers.sort();
        layers
    }
}

/// `[orders]` (compat §14): the `enabled` list that arms imported orders.
/// An imported order is INERT until named here — the money invariant.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrdersSection {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enabled: Vec<String>,
}

impl OrdersSection {
    fn is_default(&self) -> bool {
        self.enabled.is_empty()
    }
}

/// `[agent_defaults]` (compat §5.2): model/permission_mode/tools are
/// operator-owned, never pack-owned. No resolvable `tools` → no spawn.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentDefaults {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
}

impl AgentDefaults {
    fn is_default(&self) -> bool {
        *self == AgentDefaults::default()
    }
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
        // Friendly rewrite for the removed `packs = [...]` key BEFORE
        // `deny_unknown_fields` rejects it with a generic unknown-field
        // error (component §13): a local pack is now an import whose
        // source is a path.
        let doc: toml::Value =
            toml::from_str(text).map_err(|e| CoreError::Config(e.to_string()))?;
        if doc.get("packs").is_some() {
            return Err(CoreError::Config(
                "`packs = [...]` was removed. Rewrite each pack as an import:\n  \
                 [imports.<name>]\n  source = \"<path-or-url>\"\n\
                 (a local pack is an import whose source is a path — component spec §13)"
                    .to_owned(),
            ));
        }
        let cfg: CampConfig = toml::from_str(text).map_err(|e| CoreError::Config(e.to_string()))?;
        if cfg.dispatch.max_workers == 0 {
            // A typo'd cap must not silently disable dispatch (PR #14
            // review finding 5).
            return Err(CoreError::Config(
                "[dispatch] max_workers must be at least 1 (0 would disable dispatch entirely)"
                    .to_owned(),
            ));
        }
        // Same law for [patrol]: a typo'd threshold must not become dead
        // config (validation only; campd resolves the typed values later).
        crate::patrol::PatrolConfig::from_section(&cfg.patrol)?;
        // And for [dispatch] exec_timeout (issue #55): the subprocess
        // bound must resolve or the config is refused.
        cfg.dispatch.exec_timeout()?;
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

    /// D7 (operator ruling, issue #80): a LOCAL-path import is layered IN
    /// PLACE — resolvers read the operator's own directory, resolved relative
    /// to camp.toml. It is never fetched and never copied under `imports/`
    /// (component §5's layout diagram: "local path: layered in place / no
    /// fetch, no lock entry"). A git-backed import keeps the DERIVED
    /// materialization path. Mutating `layer_dir` to return the materialized
    /// path for a local source turns this red.
    #[test]
    fn a_local_import_layers_in_place_and_a_git_import_layers_under_imports() {
        let root = Path::new("/camp");
        let local = ImportDecl {
            source: "../packs/house".to_owned(),
            subpath: None,
            version: None,
            trust_exec: false,
            skills: None,
        };
        assert!(local.is_local());
        assert_eq!(
            local.layer_dir(root, "house"),
            PathBuf::from("/camp/../packs/house"),
            "a local source is read in place, relative to camp.toml"
        );

        let git = ImportDecl {
            source: "https://github.com/Liquescent-Development/gascamp".to_owned(),
            subpath: Some("packs/starter".to_owned()),
            version: None,
            trust_exec: false,
            skills: None,
        };
        assert!(!git.is_local());
        assert_eq!(
            git.layer_dir(root, "starter"),
            PathBuf::from("/camp/imports/starter"),
            "a git source is materialized; its location is DERIVED, never stored"
        );
    }

    /// D8 (operator ruling, issue #80): a DIRECT import OVERRIDES a transitive
    /// one for the same binding (§7.1 — the operator's binding always wins;
    /// gc's own rule, pack.go:335-340). The direct import owns
    /// `imports/<binding>/`, while the transitive layer persists under a
    /// SEPARATE path — so a direct override never clobbers the transitive
    /// FORMULA layers that the corpus's `extends = [...]` depend on.
    #[test]
    fn a_direct_import_and_a_transitive_layer_of_the_same_binding_have_disjoint_dirs() {
        let root = Path::new("/camp");
        let direct = ImportDecl {
            source: "https://example.com/gc".to_owned(),
            subpath: None,
            version: None,
            trust_exec: false,
            skills: None,
        };
        assert_eq!(
            direct.layer_dir(root, "gc"),
            PathBuf::from("/camp/imports/gc")
        );
        assert_eq!(
            transitive_layer_dir(root, "gc"),
            PathBuf::from("/camp/imports/.transitive/gc"),
            "the transitive layer is materialized OUTSIDE the direct binding's dir"
        );
        assert_ne!(
            direct.layer_dir(root, "gc"),
            transitive_layer_dir(root, "gc")
        );
    }

    /// The transitive sentinel can never collide with a real binding: binding
    /// names are validated `[A-Za-z0-9_-]+`, so none can equal `.transitive`.
    #[test]
    fn the_transitive_sentinel_is_not_a_legal_binding_name() {
        assert!(
            !TRANSITIVE_DIR
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
            "{TRANSITIVE_DIR:?} must be unspellable as a binding, or a pack could shadow it"
        );
    }

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

    /// Issue #55: every subprocess campd runs inline on its event loop is
    /// bounded by `[dispatch] exec_timeout` — defaulted, overridable
    /// (visible config, the fake-agent precedent), resolved to a std
    /// Duration for the deadline arithmetic.
    #[test]
    fn dispatch_exec_timeout_defaults_and_resolves() {
        let cfg = CampConfig::parse("[camp]\nname=\"d\"\n").unwrap();
        assert_eq!(cfg.dispatch.exec_timeout, "60s");
        assert_eq!(
            cfg.dispatch.exec_timeout().unwrap(),
            std::time::Duration::from_secs(60)
        );
        let cfg =
            CampConfig::parse("[camp]\nname=\"d\"\n[dispatch]\nexec_timeout = \"2s\"\n").unwrap();
        assert_eq!(
            cfg.dispatch.exec_timeout().unwrap(),
            std::time::Duration::from_secs(2)
        );
    }

    /// A typo'd exec_timeout must not become dead config (the max_workers
    /// / [patrol] law): rejected at parse, naming the key.
    #[test]
    fn dispatch_exec_timeout_rejects_zero_negative_and_junk_at_parse() {
        for bad in ["0s", "-5s", "banana"] {
            let err = CampConfig::parse(&format!(
                "[camp]\nname=\"d\"\n[dispatch]\nexec_timeout = \"{bad}\"\n"
            ))
            .unwrap_err();
            assert!(
                err.to_string().contains("exec_timeout"),
                "{bad:?}: the error must name the failing key: {err}"
            );
        }
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
            imports: std::collections::BTreeMap::new(),
            orders_section: OrdersSection::default(),
            agent_defaults: AgentDefaults::default(),
            dispatch: DispatchConfig::default(),
            patrol: PatrolSection::default(),
            root: None,
        };
        let text = toml::to_string(&cfg).unwrap();
        assert_eq!(CampConfig::parse(&text).unwrap(), cfg);
    }

    // ---- Phase 8: [dispatch], per-rig default_agent ----------------------

    #[test]
    fn dispatch_and_imports_parse_with_defaults() {
        let cfg = CampConfig::parse(
            r#"
[camp]
name = "dev"

[[rigs]]
name = "gascity"
path = "/code/gascity"
prefix = "gc"
default_agent = "rigger"

[imports.starter]
source = "packs/starter"

[dispatch]
max_workers = 3
command = "tests/fake-agent.sh"
default_agent = "dev"
"#,
        )
        .unwrap();
        assert_eq!(cfg.imports["starter"].source, "packs/starter");
        assert!(!cfg.imports["starter"].trust_exec);
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
        assert!(cfg.imports.is_empty());
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
        // [dispatch]/[imports]/[orders]/[agent_defaults] blocks the user
        // never wrote.
        let cfg = CampConfig::parse("[camp]\nname = \"dev\"\n").unwrap();
        let text = toml::to_string(&cfg).unwrap();
        assert!(!text.contains("dispatch"), "text was: {text}");
        assert!(!text.contains("imports"), "text was: {text}");
        assert!(!text.contains("orders"), "text was: {text}");
        assert!(!text.contains("agent_defaults"), "text was: {text}");
    }

    // ---- compat phase 1: [imports.*], [orders] enabled, [agent_defaults] --

    #[test]
    fn imports_orders_enabled_and_agent_defaults_parse() {
        let cfg = CampConfig::parse(
            r#"
[camp]
name = "dev"

[imports.bmad]
source = "https://github.com/gastownhall/gascity-packs"
subpath = "bmad"
version = "sha:deadbeef"

[imports.gc]
source = "../local/roles"
trust_exec = true
skills = false

[orders]
enabled = ["bmad.nightly", "gc.triage"]

[agent_defaults]
model = "sonnet"
permission_mode = "acceptEdits"
tools = ["Read", "Edit", "Bash", "Skill"]
"#,
        )
        .unwrap();
        let bmad = &cfg.imports["bmad"];
        assert_eq!(bmad.source, "https://github.com/gastownhall/gascity-packs");
        assert_eq!(bmad.subpath.as_deref(), Some("bmad"));
        assert_eq!(bmad.version.as_deref(), Some("sha:deadbeef"));
        assert!(!bmad.trust_exec);
        let gc = &cfg.imports["gc"];
        assert!(gc.trust_exec);
        assert_eq!(gc.skills, Some(false));
        assert_eq!(
            cfg.orders_section.enabled,
            vec!["bmad.nightly", "gc.triage"]
        );
        assert_eq!(cfg.agent_defaults.model.as_deref(), Some("sonnet"));
        assert_eq!(
            cfg.agent_defaults.tools.as_deref().unwrap(),
            ["Read", "Edit", "Bash", "Skill"]
        );
    }

    #[test]
    fn legacy_packs_key_is_a_specific_rewrite_error() {
        let err =
            CampConfig::parse("packs = [\"packs/starter\"]\n[camp]\nname = \"d\"\n").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("packs"), "{msg}");
        assert!(msg.contains("[imports."), "must show the rewrite: {msg}");
    }

    #[test]
    fn agent_defaults_reject_unknown_keys() {
        assert!(CampConfig::parse("[camp]\nname=\"d\"\n[agent_defaults]\nbogus = 1\n").is_err());
    }

    #[test]
    fn load_records_the_camp_root() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("camp.toml");
        std::fs::write(&path, "[camp]\nname = \"dev\"\n").unwrap();
        let cfg = CampConfig::load(&path).unwrap();
        assert_eq!(cfg.root.as_deref(), Some(dir.path()));
    }

    // ---- Phase 11: [patrol] ----------------------------------------------

    #[test]
    fn patrol_section_parses_with_defaults_and_overrides() {
        let cfg = CampConfig::parse("[camp]\nname=\"d\"\n").unwrap();
        assert_eq!(cfg.patrol.stall_after, "10m");
        assert_eq!(cfg.patrol.restart_budget, 2);
        assert_eq!(cfg.patrol.release_grace, "30s");
        let cfg = CampConfig::parse(
            "[camp]\nname=\"d\"\n[patrol]\nstall_after=\"90s\"\nrestart_budget=1\nrelease_grace=\"500ms\"\n",
        )
        .unwrap();
        assert_eq!(cfg.patrol.stall_after, "90s");
        assert_eq!(cfg.patrol.restart_budget, 1);
        assert_eq!(cfg.patrol.release_grace, "500ms");
    }

    #[test]
    fn bad_patrol_durations_are_rejected_at_parse() {
        // A typo'd threshold must not silently become dead patrol config
        // (the max_workers precedent, PR #14 review finding 5).
        for toml in [
            "[camp]\nname=\"d\"\n[patrol]\nstall_after=\"0s\"\n",
            "[camp]\nname=\"d\"\n[patrol]\nstall_after=\"nope\"\n",
            "[camp]\nname=\"d\"\n[patrol]\nrelease_grace=\"-1s\"\n",
        ] {
            let err = CampConfig::parse(toml).unwrap_err();
            assert!(err.to_string().contains("patrol"), "{toml}: {err}");
        }
    }

    #[test]
    fn unknown_patrol_key_is_rejected() {
        assert!(CampConfig::parse("[camp]\nname=\"d\"\n[patrol]\nbogus=1\n").is_err());
    }

    #[test]
    fn patrol_defaults_do_not_pollute_serialization() {
        let cfg = CampConfig::parse("[camp]\nname = \"dev\"\n").unwrap();
        assert!(!toml::to_string(&cfg).unwrap().contains("patrol"));
    }
}

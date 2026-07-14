//! The formula LAYER STACK (compat §7.2): the ordered set of places a formula
//! name, a parent, or an asset resolves through.
//!
//! Order is lowest → highest priority:
//!
//! ```text
//!   transitive content layers  <  direct imports  <  <root>/formulas/
//! ```
//!
//! It is the same stack compat-1's `orders::resolve_formula` already walks, and
//! [`FormulaLayers::formula_path`] DELEGATES to it rather than growing a second
//! resolver that could disagree with the first.
//!
//! Two things layering is load-bearing for, and both are easy to get wrong:
//!
//! * **Assets shadow, and the WINNER decides containment.** `../assets/<rel>`
//!   searches every layer and the HIGHEST match wins (gc `winningAssetPath`,
//!   `parser.go:859-875`), so a pack can override prose while inheriting
//!   structure. 32 cross-pack `extends` edges inherit a step whose asset lives
//!   in the PARENT's pack — anchoring containment on the declaring formula's
//!   pack would call every one of them an escape.
//! * **`description_file` is NEVER `{name}`-substituted** (D5). 121 corpus asset
//!   files are named, on disk, literally `{target}.*.md` — with the braces.

use std::path::{Path, PathBuf};

use crate::config::CampConfig;
use crate::error::CoreError;
use crate::formula::keys::Origin;

/// gc's documented asset form (`descriptionAssetRelPath`, `parser.go:964`).
const ASSET_PREFIX: &str = "../assets/";

#[derive(Debug, Clone)]
pub struct Layer {
    /// The import binding, or `""` for `<root>/formulas/`.
    pub binding: String,
    /// The PACK ROOT — the dir holding `formulas/` and `assets/`.
    pub pack_root: PathBuf,
    pub origin: Origin,
}

/// The layer stack, lowest priority first.
#[derive(Debug, Clone)]
pub struct FormulaLayers {
    layers: Vec<Layer>,
    /// Carried so `formula_path` can delegate to compat-1's resolver, which is
    /// the single source of truth for name → path.
    cfg: Option<CampConfig>,
}

impl FormulaLayers {
    /// The real stack: every import layer plus `<root>/formulas/`.
    pub fn from_config(cfg: &CampConfig, root: &Path) -> Result<Self, CoreError> {
        let mut layers = Vec::new();
        // Lowest first: transitive, then direct imports, then camp-local.
        for (binding, dir) in cfg.transitive_layers()? {
            layers.push(Layer {
                binding,
                pack_root: dir,
                origin: Origin::Imported,
            });
        }
        for (binding, dir) in cfg.import_layers() {
            layers.push(Layer {
                binding,
                pack_root: dir,
                origin: Origin::Imported,
            });
        }
        layers.push(Layer {
            binding: String::new(),
            pack_root: root.to_path_buf(),
            origin: Origin::CampLocal,
        });
        Ok(Self {
            layers,
            cfg: Some(cfg.clone()),
        })
    }

    /// One file, no layers — `parse_and_validate`'s stack. The formula's own
    /// directory's parent is its "pack root" for containment, so a bare file in
    /// a temp dir still resolves its own relative assets.
    pub fn local_only(path: &Path) -> Self {
        let pack_root = path
            .parent()
            .and_then(Path::parent)
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        Self {
            layers: vec![Layer {
                binding: String::new(),
                pack_root,
                origin: Origin::CampLocal,
            }],
            cfg: None,
        }
    }

    pub fn layers(&self) -> &[Layer] {
        &self.layers
    }

    /// D2′ — which strictness tier does this file sit in? A file under an
    /// IMPORTED pack's root is a third-party artifact camp merely reads; a file
    /// anywhere else is the operator's own, where a typo is a bug.
    pub fn origin_of(&self, path: &Path) -> Origin {
        match self.owning_layer(path) {
            Some(layer) => layer.origin,
            None => Origin::CampLocal,
        }
    }

    /// The highest-priority layer whose pack root contains `path`.
    fn owning_layer(&self, path: &Path) -> Option<&Layer> {
        self.layers
            .iter()
            .rev()
            .find(|l| path.starts_with(&l.pack_root))
    }

    /// A formula's path, by BARE NAME, through the layers. Delegates to
    /// compat-1's `orders::resolve_formula`, which already handles both file
    /// spellings (`<n>.toml` and `<n>.formula.toml`) and the tier precedence.
    pub fn formula_path(&self, name: &str) -> Result<PathBuf, CoreError> {
        let cfg = self.cfg.as_ref().ok_or_else(|| {
            CoreError::Config(format!(
                "formula {name:?} cannot be resolved: this is a no-layer stack \
                 (parse_and_validate); an `extends` needs a real camp with imports"
            ))
        })?;
        crate::orders::resolve_formula(cfg, name)
    }

    /// Resolve a `description_file` value to a real path.
    ///
    /// `../assets/<rel>` searches every layer, HIGHEST wins (gc
    /// `winningAssetPath`). Anything else resolves against the formula file's
    /// own directory (gc `readDescriptionFile`, `parser.go:846-851`).
    ///
    /// **Containment (security, and camp needs it where gc does not).** gc's
    /// non-asset branch is a bare `filepath.Join(baseDir, path)`. Camp imports
    /// arbitrary third-party packs, so a pack could write
    /// `description_file = "../../../../.ssh/id_rsa"` and have it inlined into a
    /// bead description that a tool-enabled worker then reads. Camp canonicalises
    /// and refuses any path outside the pack root — and the root it checks is
    /// the WINNING LAYER's, not the declaring formula's.
    pub fn asset_path(&self, raw: &str, base_dir: &Path) -> Result<PathBuf, CoreError> {
        // Canonicalise the ROOT and the BASE before building any candidate: on
        // macOS `/var` is a symlink to `/private/var`, so a candidate built from
        // an uncanonicalised base would never look contained by a canonicalised
        // root, and every asset in a temp dir would read as an escape.
        let canon = |p: &Path, what: &str| -> Result<PathBuf, CoreError> {
            p.canonicalize().map_err(|e| {
                CoreError::Formula(format!(
                    "description_file {raw:?}: cannot resolve the {what} {}: {e}",
                    p.display()
                ))
            })
        };

        let (candidate, root) = match asset_rel(raw) {
            Some(rel) => {
                // Lowest → highest, LAST match wins (gc `winningAssetPath`).
                let mut winner = None;
                for layer in &self.layers {
                    let candidate = layer.pack_root.join("assets").join(&rel);
                    if candidate.is_file() {
                        winner = Some((candidate, layer.pack_root.clone()));
                    }
                }
                let (_, pack_root) = winner.ok_or_else(|| {
                    CoreError::Formula(format!(
                        "description_file {raw:?}: no formula layer ships `assets/{rel}` \
                         (searched {} layer(s))",
                        self.layers.len()
                    ))
                })?;
                // Rebuild the candidate off the CANONICAL root, so the
                // containment comparison is canonical-vs-canonical on both sides.
                let root = canon(&pack_root, "pack root")?;
                (root.join("assets").join(&rel), root)
            }
            None => {
                let pack_root = self
                    .owning_layer(base_dir)
                    .map_or_else(|| base_dir.to_path_buf(), |l| l.pack_root.clone());
                let root = canon(&pack_root, "pack root")?;
                let base = canon(base_dir, "formula directory")?;
                (base.join(raw), root)
            }
        };
        let outside = |p: &Path| {
            CoreError::Formula(format!(
                "description_file {raw:?} resolves to {}, which is outside the pack root {} — \
                 camp refuses to inline a file a pack reached out of its own tree to name",
                p.display(),
                root.display()
            ))
        };

        // TWO containment checks, and both are load-bearing.
        //
        // LEXICAL first, because `canonicalize` requires the file to EXIST: an
        // escape naming a path that happens not to be there would otherwise fail
        // as "cannot resolve" and never be reported as what it is.
        let lexical = lexical_normalize(&candidate);
        if !lexical.starts_with(&root) {
            return Err(outside(&lexical));
        }
        // CANONICAL second, because a lexical check cannot see a SYMLINK. A pack
        // shipping `assets/x.md -> /etc/passwd` is lexically contained and really
        // is not.
        let resolved = candidate.canonicalize().map_err(|e| {
            CoreError::Formula(format!(
                "description_file {raw:?}: cannot resolve {}: {e}",
                candidate.display()
            ))
        })?;
        if !resolved.starts_with(&root) {
            return Err(outside(&resolved));
        }
        Ok(resolved)
    }
}

/// Resolve `.` and `..` WITHOUT touching the filesystem. `Path::canonicalize`
/// cannot do this job: it requires the path to exist, and a containment check
/// that only runs for files that happen to be present is not a containment
/// check.
fn lexical_normalize(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// gc's `descriptionAssetRelPath` (`parser.go:964-975`): the documented
/// `../assets/<rel>` form, with `..`-escapes out of it rejected.
fn asset_rel(raw: &str) -> Option<String> {
    let path = raw.replace('\\', "/");
    let rel = path.strip_prefix(ASSET_PREFIX)?;
    if rel.is_empty() || rel == "." || rel.starts_with("../") {
        return None;
    }
    Some(rel.to_owned())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn the_asset_form_is_gcs_documented_prefix_only() {
        assert_eq!(
            asset_rel("../assets/workflows/x.md").as_deref(),
            Some("workflows/x.md")
        );
        // Not the asset form: resolves against the formula's own dir instead.
        assert_eq!(asset_rel("prompts/x.md"), None);
        assert_eq!(asset_rel("./x.md"), None);
        // An escape THROUGH the asset form is not the asset form.
        assert_eq!(asset_rel("../assets/../../etc/passwd"), None);
        assert_eq!(asset_rel("../assets/"), None);
    }

    #[test]
    fn a_brace_in_an_asset_path_is_carried_verbatim() {
        // 121 corpus asset files are named, on disk, literally `{target}.*.md`.
        // `description_file` is never `{name}`-substituted (D5) — if it were,
        // all 130 of these would stop resolving.
        assert_eq!(
            asset_rel("../assets/workflows/f/{target}.apply.md").as_deref(),
            Some("workflows/f/{target}.apply.md")
        );
    }
}

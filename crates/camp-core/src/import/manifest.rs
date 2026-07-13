//! `pack.toml` (component §7): a pack's required manifest. `[pack].name` +
//! `[pack].schema` (≤ 2) are required; `version` is NOT required (gastown
//! ships without it — component §7.4). `[imports.*]` reuses `ImportDecl` for
//! pack-level transitive imports. The top level is NON-strict (gc tolerates
//! extra top-level tables like `[catalog]`); the `[pack]` table IS strict.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::config::ImportDecl;
use crate::error::CoreError;

/// A pack manifest. The top level tolerates unknown tables (gc packs carry
/// `[catalog]` etc.); only `[pack]` is captured and validated strictly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackManifest {
    pub pack: PackMeta,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub imports: BTreeMap<String, ImportDecl>,
}

/// The strict `[pack]` table. `version` is optional (gastown ships without).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackMeta {
    pub name: String,
    pub schema: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// Read a pack's manifest. Missing `pack.toml` → "not a pack". A schema
/// above 2 → a named error. The binding for errors is the directory's name.
pub fn read_manifest(pack_dir: &Path) -> Result<PackManifest, CoreError> {
    let manifest_path = pack_dir.join("pack.toml");
    let binding = pack_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("<pack>")
        .to_owned();
    let text = match std::fs::read_to_string(&manifest_path) {
        Ok(t) => t,
        Err(_) => {
            return Err(CoreError::Import {
                binding,
                reason: "no pack.toml — not a pack".to_owned(),
            });
        }
    };
    let manifest: PackManifest = toml::from_str(&text).map_err(|e| CoreError::Import {
        binding: binding.clone(),
        reason: format!("pack.toml is not valid TOML: {e}"),
    })?;
    if manifest.pack.schema > 2 {
        return Err(CoreError::Import {
            binding,
            reason: format!(
                "pack.toml schema {} is unsupported (this build supports <= 2)",
                manifest.pack.schema
            ),
        });
    }
    Ok(manifest)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn reads_pack_and_optional_pack_level_imports() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pack.toml"),
            "[pack]\nname = \"bmad\"\nversion = \"0.1.0\"\nschema = 2\n\n[imports.gc]\nsource = \"../gascity\"\n").unwrap();
        let m = read_manifest(dir.path()).unwrap();
        assert_eq!(m.pack.name, "bmad");
        assert_eq!(m.pack.schema, 2);
        assert_eq!(m.imports["gc"].source, "../gascity");
    }
    #[test]
    fn version_is_not_required() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("pack.toml"),
            "[pack]\nname = \"gastown\"\nschema = 2\n",
        )
        .unwrap();
        assert!(read_manifest(dir.path()).is_ok());
    }
    #[test]
    fn schema_above_2_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("pack.toml"),
            "[pack]\nname = \"x\"\nschema = 3\n",
        )
        .unwrap();
        assert!(
            read_manifest(dir.path())
                .unwrap_err()
                .to_string()
                .contains("schema")
        );
    }
    #[test]
    fn missing_manifest_is_not_a_pack() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            read_manifest(dir.path())
                .unwrap_err()
                .to_string()
                .contains("pack.toml")
        );
    }
    #[test]
    fn strict_pack_table_but_tolerant_top_level() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("pack.toml"),
            "[pack]\nname=\"x\"\nschema=2\nbogus=1\n",
        )
        .unwrap();
        assert!(read_manifest(dir.path()).is_err());
        std::fs::write(
            dir.path().join("pack.toml"),
            "[pack]\nname=\"x\"\nschema=2\n[catalog]\nx=1\n",
        )
        .unwrap();
        assert!(read_manifest(dir.path()).is_ok());
    }
}

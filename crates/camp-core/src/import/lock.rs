//! `packs.lock` (component §5): the pinned-import manifest written beside
//! `camp.toml`. One entry per materialized import (direct + transitive, the
//! transitive ones carrying `via = <declaring-binding>`). The location is
//! NEVER stored — an import always materializes at `<root>/imports/<name>/`,
//! so storing it would be a write-anywhere hole (component §5). `schema = 1`
//! is the only accepted version; a different value is a hard error naming it.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::CoreError;

/// The lock file: `schema = 1` plus one `[import]]` entry per materialized
/// import (direct or transitive).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PacksLock {
    pub schema: i64,
    #[serde(default, rename = "import")]
    pub imports: Vec<LockEntry>,
}

/// One pinned import. `via` names the declaring binding for a transitive
/// import (`None` for a direct import).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockEntry {
    pub name: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subpath: Option<String>,
    pub version: String,
    pub commit: String,
    pub fetched: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
}

impl PacksLock {
    pub const SCHEMA: i64 = 1;

    /// Read a lock file. Missing → an empty schema-1 lock (a fresh camp).
    /// A `schema` other than 1 is a hard error naming the value.
    pub fn read(path: &Path) -> Result<PacksLock, CoreError> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(PacksLock::empty());
            }
            Err(e) => {
                return Err(CoreError::Import {
                    binding: path.display().to_string(),
                    reason: format!("cannot read packs.lock: {e}"),
                });
            }
        };
        let lock: PacksLock =
            toml::from_str(&text).map_err(|e| CoreError::Import {
                binding: path.display().to_string(),
                reason: format!("packs.lock is not valid TOML: {e}"),
            })?;
        if lock.schema != Self::SCHEMA {
            return Err(CoreError::Import {
                binding: path.display().to_string(),
                reason: format!(
                    "packs.lock schema {} is unsupported (this build supports {}); re-import to regenerate",
                    lock.schema, Self::SCHEMA
                ),
            });
        }
        Ok(lock)
    }

    /// Write the lock file. `schema = 1` is always emitted first.
    pub fn write(&self, path: &Path) -> Result<(), CoreError> {
        let text = toml::to_string(self).map_err(|e| CoreError::Import {
            binding: path.display().to_string(),
            reason: format!("cannot serialize packs.lock: {e}"),
        })?;
        std::fs::write(path, text).map_err(|e| CoreError::Import {
            binding: path.display().to_string(),
            reason: format!("cannot write packs.lock: {e}"),
        })
    }

    /// The entry for a binding, or `None`.
    pub fn entry(&self, name: &str) -> Option<&LockEntry> {
        self.imports.iter().find(|e| e.name == name)
    }

    fn empty() -> Self {
        PacksLock {
            schema: Self::SCHEMA,
            imports: Vec::new(),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn lock_roundtrips_with_via_provenance() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("packs.lock");
        let lock = PacksLock {
            schema: 1,
            imports: vec![
                LockEntry {
                    name: "bmad".into(),
                    source: "https://x/repo".into(),
                    subpath: Some("bmad".into()),
                    version: "sha:abc".into(),
                    commit: "abc".into(),
                    fetched: "2026-07-12T00:00:00Z".into(),
                    via: None,
                },
                LockEntry {
                    name: "gc".into(),
                    source: "https://x/repo".into(),
                    subpath: Some("gascity".into()),
                    version: "sha:abc".into(),
                    commit: "abc".into(),
                    fetched: "2026-07-12T00:00:00Z".into(),
                    via: Some("bmad".into()),
                },
            ],
        };
        lock.write(&p).unwrap();
        assert_eq!(PacksLock::read(&p).unwrap(), lock);
        assert_eq!(
            PacksLock::read(&p).unwrap().entry("gc").unwrap().via.as_deref(),
            Some("bmad")
        );
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("schema = 1") && !text.contains("location"));
    }
    #[test]
    fn missing_lock_reads_as_empty_schema_1() {
        let dir = tempfile::tempdir().unwrap();
        let lock = PacksLock::read(&dir.path().join("packs.lock")).unwrap();
        assert!(lock.schema == 1 && lock.imports.is_empty());
    }
    #[test]
    fn unknown_schema_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("packs.lock");
        std::fs::write(&p, "schema = 2\n").unwrap();
        assert!(PacksLock::read(&p).unwrap_err().to_string().contains("schema"));
    }
}
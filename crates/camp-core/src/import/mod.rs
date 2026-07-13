//! Pack import machinery (compat §7): the binding namespace. The umbrella
//! spec's compat phase 1 replaces the `packs = [...]` list with explicit
//! imports — each materialized under `<root>/imports/<binding>/` and
//! qualified as `<binding>.<name>`.
//!
//! This module is the pure camp-core half: source grammar, the lock model,
//! the pack manifest, materialization, transitive resolution, skills
//! install, and the `trust_exec` inventory. The camp binary half (`camp
//! import` verbs + the hardened git subprocess) lives in `crates/camp/src/cmd/import.rs`.

pub mod inventory;
pub mod lock;
pub mod manifest;
pub mod materialize;
pub mod skills;
pub mod source;

use std::collections::{BTreeMap, BTreeSet};

use crate::error::CoreError;

/// One resolved import — a direct import or a transitively-discovered one.
/// Transitive imports reuse the declaring import's repo + reference and
/// carry `via = Some(declaring binding)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedImport {
    pub binding: String,
    pub source: String,
    pub subpath: Option<String>,
    pub reference: Option<String>,
    pub via: Option<String>,
    pub is_local: bool,
}

/// Resolve an import graph to depth 1 (umbrella §7.2, KNOWN-DEFECTS C3).
///
/// For each direct import, read its `pack.toml` `[imports.*]`. A transitive
/// source MUST be a relative path anchored at the declaring pack's subpath
/// within its own repo/commit (transitive subpath = normalized
/// `<declaring subpath>/<relative>`; a path escaping the repo root is a hard
/// error). A remote transitive source is refused (constrained to the
/// declaring repo, umbrella §13). Camp materializes the transitive pack
/// ITSELF, deduped by `(repo, reference, subpath)` with `via = declaring
/// binding`. A transitive binding that two different `(repo, reference,
/// subpath)` triples claim is a hard error naming the binding. A
/// transitively-imported pack that itself declares `[imports.*]` is refused
/// (depth >1).
pub fn resolve_transitive(
    direct: &[ResolvedImport],
    manifest_of: &dyn Fn(&ResolvedImport) -> Result<manifest::PackManifest, CoreError>,
) -> Result<Vec<ResolvedImport>, CoreError> {
    // Phase A: collect transitive declarations, enforcing the remote/escape/
    // clash/dedupe rules. Clashes surface here, before the depth check, so
    // a binding clash is reported as the binding, not as a depth symptom.
    let mut transitive: Vec<ResolvedImport> = Vec::new();
    let mut seen_keys: BTreeSet<(String, Option<String>, Option<String>)> = BTreeSet::new();
    let mut binding_to_key: BTreeMap<String, (String, Option<String>, Option<String>)> =
        BTreeMap::new();
    for d in direct {
        let manifest = manifest_of(d)?;
        for (trans_binding, decl) in &manifest.imports {
            if is_remote_transitive(&decl.source) {
                return Err(CoreError::Import {
                    binding: trans_binding.clone(),
                    reason: format!(
                        "transitive import {trans_binding:?} must stay within the declaring repo; \
                         remote/absolute source {:?} refused (umbrella §13)",
                        decl.source
                    ),
                });
            }
            let declaring_sub = d.subpath.clone().unwrap_or_default();
            let trans_sub = normalize_subpath(&declaring_sub, &decl.source)?;
            let key = (
                d.source.clone(),
                d.reference.clone(),
                Some(trans_sub.clone()),
            );
            if let Some(existing) = binding_to_key.get(trans_binding) {
                if existing != &key {
                    return Err(CoreError::Import {
                        binding: trans_binding.clone(),
                        reason: format!(
                            "binding {trans_binding:?} clash: declared by two different \
                             (repo, ref, subpath): {existing:?} vs {key:?}"
                        ),
                    });
                }
                // same binding, same key: fall through to the content dedupe.
            } else {
                binding_to_key.insert(trans_binding.clone(), key.clone());
            }
            if !seen_keys.insert(key.clone()) {
                continue; // same content (repo, ref, subpath) → dedupe
            }
            transitive.push(ResolvedImport {
                binding: trans_binding.clone(),
                source: d.source.clone(),
                subpath: Some(trans_sub),
                reference: d.reference.clone(),
                via: Some(d.binding.clone()),
                is_local: d.is_local,
            });
        }
    }
    // Phase B: depth-1 — a transitive pack that declares its own [imports.*]
    // would push the graph to depth 2; refuse it.
    for t in &transitive {
        let m = manifest_of(t)?;
        if !m.imports.is_empty() {
            return Err(CoreError::Import {
                binding: t.binding.clone(),
                reason: format!(
                    "transitive import {:?} declares its own [imports.*] — depth >1 refused",
                    t.binding
                ),
            });
        }
    }
    let mut all = direct.to_vec();
    all.extend(transitive);
    Ok(all)
}

/// A transitive source must be a relative path within the declaring repo:
/// not absolute, not a URL, not a git ssh shorthand, not an `ext::` transport.
fn is_remote_transitive(source: &str) -> bool {
    source.starts_with('/')
        || source.contains("://")
        || source.starts_with("git@")
        || source.contains("::")
}

/// Lexically normalize `<declaring_subpath>/<relative>` into a single
/// subpath, applying `..` against the declaring components. A `..` that
/// pops past the repo root escapes — a hard error.
fn normalize_subpath(declaring_sub: &str, relative: &str) -> Result<String, CoreError> {
    let combined = if declaring_sub.is_empty() {
        relative.to_owned()
    } else {
        format!("{declaring_sub}/{relative}")
    };
    let mut out: Vec<&str> = Vec::new();
    for part in combined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if out.pop().is_none() {
                    return Err(CoreError::Import {
                        binding: relative.to_owned(),
                        reason: format!("transitive source {relative:?} escapes the repo root"),
                    });
                }
            }
            other => out.push(other),
        }
    }
    Ok(out.join("/"))
}
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::import::manifest::{PackManifest, PackMeta};

    fn imp(binding: &str, subpath: &str) -> ResolvedImport {
        ResolvedImport {
            binding: binding.into(),
            source: "file:///r".into(),
            subpath: Some(subpath.into()),
            reference: Some("c1".into()),
            via: None,
            is_local: false,
        }
    }
    fn manifest(name: &str, gc_source: Option<&str>) -> PackManifest {
        let mut m = std::collections::BTreeMap::new();
        if let Some(s) = gc_source {
            m.insert(
                "gc".to_string(),
                crate::config::ImportDecl {
                    source: s.into(),
                    subpath: None,
                    version: None,
                    trust_exec: false,
                    skills: None,
                },
            );
        }
        PackManifest {
            pack: PackMeta {
                name: name.into(),
                schema: 2,
                description: None,
                version: None,
            },
            imports: m,
        }
    }

    #[test]
    fn transitive_gascity_is_materialized_and_deduped() {
        let direct = vec![imp("bmad", "bmad"), imp("gstack", "gstack")];
        let mo = |i: &ResolvedImport| {
            Ok(manifest(
                &i.subpath.clone().unwrap(),
                if i.subpath.as_deref() == Some("gascity") {
                    None
                } else {
                    Some("../gascity")
                },
            ))
        };
        let all = resolve_transitive(&direct, &mo).unwrap();
        let gascity: Vec<_> = all
            .iter()
            .filter(|i| i.subpath.as_deref() == Some("gascity"))
            .collect();
        assert_eq!(gascity.len(), 1, "deduped");
        assert!(gascity[0].via.is_some());
        assert_eq!(gascity[0].binding, "gc");
    }
    #[test]
    fn relative_source_escaping_repo_root_is_hard_error() {
        let direct = vec![imp("bmad", "bmad")];
        let mo = |_: &ResolvedImport| Ok(manifest("bmad", Some("../../etc")));
        assert!(
            resolve_transitive(&direct, &mo)
                .unwrap_err()
                .to_string()
                .to_lowercase()
                .contains("escape")
        );
    }
    #[test]
    fn depth_2_transitive_import_is_refused() {
        let direct = vec![imp("a", "a")];
        let mo = |i: &ResolvedImport| {
            Ok(manifest(
                &i.subpath.clone().unwrap(),
                Some(if i.subpath.as_deref() == Some("a") {
                    "../b"
                } else {
                    "../c"
                }),
            ))
        };
        assert!(
            resolve_transitive(&direct, &mo)
                .unwrap_err()
                .to_string()
                .contains("depth")
        );
    }
    #[test]
    fn transitive_binding_clash_is_a_hard_error() {
        // two direct imports whose transitive `gc` bindings point at DIFFERENT subpaths
        let direct = vec![imp("a", "a"), imp("b", "b")];
        let mo = |i: &ResolvedImport| {
            Ok(manifest(
                &i.subpath.clone().unwrap(),
                Some(if i.subpath.as_deref() == Some("a") {
                    "../x"
                } else {
                    "../y"
                }),
            ))
        };
        assert!(
            resolve_transitive(&direct, &mo)
                .unwrap_err()
                .to_string()
                .contains("gc")
        );
    }
}

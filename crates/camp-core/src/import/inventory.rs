//! `trust_exec` inventory (umbrella §13): scan a materialized pack for
//! executable content — a formula step's `check.path` (when
//! `check.mode == "exec"`), a step's `pre_start`/`condition` shell, and an
//! `exec`-triggered order. The inventory is what `camp import add` prints and
//! records in the `import.added` event; `ImportDecl.trust_exec` (default
//! false) is the operator's opt-in to RUN any of it. Phase 1 runs NO
//! formulas, so "executes nothing" holds by the absence of an execution
//! path — this module only INVENTORIES.

use std::path::Path;

use crate::error::CoreError;

/// One piece of executable content found in a pack. `path` is the file that
/// declares it (e.g. `formulas/build.toml`); `detail` is the executable spec
/// (the `check.path` value, the shell snippet, the order's path).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecItem {
    pub kind: &'static str,
    pub path: String,
    pub detail: String,
}

/// Inventory every executable content item in a materialized pack dir:
/// formula `check.path` (exec mode), `pre_start`, `condition`, and
/// `exec`-triggered orders. Pure scan — runs nothing.
pub fn inventory_executable(pack_dir: &Path) -> Result<Vec<ExecItem>, CoreError> {
    let mut items = Vec::new();
    let formulas_dir = pack_dir.join("formulas");
    if formulas_dir.is_dir() {
        let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(&formulas_dir)
            .map_err(|e| import_err(&formulas_dir, format!("cannot read: {e}")))?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "toml"))
            .collect();
        files.sort();
        for f in files {
            let rel = f.strip_prefix(pack_dir).unwrap_or(&f).display().to_string();
            let text = std::fs::read_to_string(&f)
                .map_err(|e| import_err(&f, format!("cannot read: {e}")))?;
            let doc: toml::Value =
                toml::from_str(&text).map_err(|e| import_err(&f, format!("invalid TOML: {e}")))?;
            if let Some(steps) = doc.get("steps").and_then(|s| s.as_array()) {
                for step in steps {
                    if let Some(check) = step.get("check").and_then(|c| c.as_table())
                        && check.get("mode").and_then(|m| m.as_str()) == Some("exec")
                        && let Some(path) = check.get("path").and_then(|p| p.as_str())
                    {
                        items.push(ExecItem {
                            kind: "check.path",
                            path: rel.clone(),
                            detail: path.to_owned(),
                        });
                    }
                    if let Some(ps) = step.get("pre_start").and_then(|p| p.as_str()) {
                        items.push(ExecItem {
                            kind: "pre_start",
                            path: rel.clone(),
                            detail: ps.to_owned(),
                        });
                    }
                    if let Some(cond) = step.get("condition").and_then(|c| c.as_str()) {
                        items.push(ExecItem {
                            kind: "condition",
                            path: rel.clone(),
                            detail: cond.to_owned(),
                        });
                    }
                }
            }
        }
    }
    let orders_dir = pack_dir.join("orders");
    if orders_dir.is_dir() {
        let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(&orders_dir)
            .map_err(|e| import_err(&orders_dir, format!("cannot read: {e}")))?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "toml"))
            .collect();
        files.sort();
        for f in files {
            let rel = f.strip_prefix(pack_dir).unwrap_or(&f).display().to_string();
            let text = std::fs::read_to_string(&f)
                .map_err(|e| import_err(&f, format!("cannot read: {e}")))?;
            let doc: toml::Value =
                toml::from_str(&text).map_err(|e| import_err(&f, format!("invalid TOML: {e}")))?;
            if let Some(order) = doc.get("order").and_then(|o| o.as_table())
                && order.get("trigger").and_then(|t| t.as_str()) == Some("exec")
            {
                let path = order.get("path").and_then(|p| p.as_str()).unwrap_or("");
                items.push(ExecItem {
                    kind: "order.exec",
                    path: rel.clone(),
                    detail: path.to_owned(),
                });
            }
        }
    }
    Ok(items)
}

fn import_err(path: &Path, reason: String) -> CoreError {
    CoreError::Import {
        binding: path.display().to_string(),
        reason,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn transitive_check_path_is_inventoried_and_untrusted_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let pack = dir.path().join("bmad");
        std::fs::create_dir_all(pack.join("formulas")).unwrap();
        std::fs::write(pack.join("formulas/build.toml"),
            "formula=\"b\"\n[[steps]]\nid=\"s\"\ntitle=\"t\"\n[steps.check]\nmode=\"exec\"\npath=\"scripts/verify.sh\"\n").unwrap();
        let items = inventory_executable(&pack).unwrap();
        assert!(
            items
                .iter()
                .any(|i| i.kind == "check.path" && i.detail.contains("verify.sh")),
            "{items:?}"
        );
        let decl = crate::config::ImportDecl {
            source: "x".into(),
            subpath: None,
            version: None,
            trust_exec: false,
            skills: None,
        };
        assert!(!decl.trust_exec, "untrusted unless the operator opts in");
    }
}

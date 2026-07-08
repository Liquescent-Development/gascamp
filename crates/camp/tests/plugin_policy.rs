#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Repo policy (Phase 12 / spec §11): the camp plugin is machinery only and
//! ships ZERO agent definitions. "If the machinery mentions a role, it is a
//! bug." Roles are pack content — the positive control asserts they live in
//! the starter pack, not the plugin.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Every path under `dir`, recursively.
fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.push(path.clone());
            walk(&path, out);
        } else {
            out.push(path);
        }
    }
}

#[test]
fn plugin_ships_zero_agent_definitions() {
    let plugin = repo_root().join("plugin");
    let mut paths = Vec::new();
    walk(&plugin, &mut paths);

    // No `agents` directory anywhere under plugin/.
    for p in &paths {
        assert!(
            p.file_name().and_then(|n| n.to_str()) != Some("agents"),
            "the plugin must ship no agents/ directory: {}",
            p.display()
        );
    }

    // The manifest must not declare an `agents` component path.
    let manifest = std::fs::read_to_string(plugin.join(".claude-plugin/plugin.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&manifest).unwrap();
    assert!(
        v.get("agents").is_none(),
        "plugin.json must not declare an `agents` component path"
    );
}

#[test]
fn roles_are_pack_content_not_machinery() {
    // Positive control: roles DO exist — as pack content.
    assert!(
        repo_root().join("packs/starter/agents/dev.md").exists(),
        "roles must live in packs, proving the plugin's emptiness is deliberate"
    );
}

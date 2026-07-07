use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use camp_core::config::{CampConfig, RigConfig};
use camp_core::event::{EventInput, EventType};
use camp_core::id::validate_prefix;
use camp_core::ledger::Ledger;

use crate::campdir::CampDir;

/// `camp rig add <path> [--prefix p] [--name n]`: register a repo as a rig.
/// Records `rig.added` (spec §13.4), then appends a `[[rigs]]` block to
/// camp.toml (decision D). camp.toml is the rig source of truth.
pub fn add(
    camp: &CampDir,
    path: PathBuf,
    prefix: Option<String>,
    name: Option<String>,
) -> Result<()> {
    let abs = std::fs::canonicalize(&path)
        .with_context(|| format!("rig path {} does not exist", path.display()))?;
    if !abs.is_dir() {
        bail!("rig path {} is not a directory", abs.display());
    }
    let name = name.unwrap_or_else(|| default_name(&abs));
    let prefix = match prefix {
        Some(p) => p,
        None => default_prefix(&name)?,
    };
    validate_prefix(&prefix).map_err(|e| anyhow::anyhow!("{e}"))?;

    let config_path = camp.config_path();

    // Serialize the whole check → append-event → write critical section
    // against other `rig add` processes with an exclusive advisory lock on
    // camp.toml (decision H). Without it, two concurrent adds could both pass
    // the duplicate check, both emit rig.added, then clobber each other's
    // camp.toml write — losing a rig from the source-of-truth file. The lock
    // is held across the load, so the loser re-reads the winner's rig and
    // fails its duplicate check cleanly. Advisory locks release on drop /
    // process exit, so a crash never leaves a stuck lock (crash-only design).
    let lock_file = std::fs::File::open(&config_path)
        .with_context(|| format!("cannot open {} to lock it", config_path.display()))?;
    lock_file
        .lock()
        .with_context(|| format!("cannot acquire exclusive lock on {}", config_path.display()))?;

    let config = CampConfig::load(&config_path)?;
    if config.rigs.iter().any(|r| r.name == name) {
        bail!("a rig named {name:?} already exists");
    }
    if config.rigs.iter().any(|r| r.prefix == prefix) {
        bail!("prefix {prefix:?} is already used by another rig");
    }

    let rig = RigConfig {
        name: name.clone(),
        path: abs.clone(),
        prefix: prefix.clone(),
    };

    let mut ledger = Ledger::open(&camp.db_path())?;
    let seq = ledger.append(EventInput {
        kind: EventType::RigAdded,
        rig: Some(name.clone()),
        actor: "cli".into(),
        bead: None,
        data: serde_json::json!({ "path": abs, "prefix": prefix }),
    })?;
    crate::daemon::socket::poke_best_effort(&camp.socket_path(), seq);
    append_rig_toml(&config_path, &rig)?;
    drop(lock_file); // release only after the write has landed

    println!("added rig {name} ({prefix}) -> {}", abs.display());
    Ok(())
}

/// `camp rig ls [--json]`: list configured rigs (read from camp.toml).
pub fn ls(camp: &CampDir, json: bool) -> Result<()> {
    let config = CampConfig::load(&camp.config_path())?;
    if json {
        println!("{}", serde_json::to_string(&config.rigs)?);
    } else {
        for r in &config.rigs {
            println!("{}\t{}\t{}", r.name, r.prefix, r.path.display());
        }
    }
    Ok(())
}

fn default_name(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("rig")
        .to_owned()
}

fn default_prefix(name: &str) -> Result<String> {
    let slug: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    if slug.chars().next().is_none_or(|c| !c.is_ascii_lowercase()) {
        bail!("cannot derive a prefix from rig name {name:?}; pass --prefix");
    }
    Ok(slug)
}

fn append_rig_toml(config_path: &Path, rig: &RigConfig) -> Result<()> {
    let fragment: BTreeMap<&str, Vec<&RigConfig>> = BTreeMap::from([("rigs", vec![rig])]);
    let block = toml::to_string(&fragment).context("cannot serialize rig entry")?;
    let mut existing = std::fs::read_to_string(config_path)
        .with_context(|| format!("cannot read {}", config_path.display()))?;
    if !existing.ends_with('\n') {
        existing.push('\n');
    }
    existing.push('\n');
    existing.push_str(&block);
    std::fs::write(config_path, existing)
        .with_context(|| format!("cannot write {}", config_path.display()))?;
    Ok(())
}

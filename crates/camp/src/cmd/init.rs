use std::path::Path;

use anyhow::{Context, Result, bail};
use camp_core::ledger::Ledger;

/// Create a new camp: `<cwd>/.camp` by default, `--camp DIR` to choose.
pub fn run(camp_flag: Option<&Path>) -> Result<()> {
    let root = match camp_flag {
        Some(dir) => dir.to_path_buf(),
        None => std::env::current_dir()
            .context("cannot determine current directory")?
            .join(".camp"),
    };
    if root.join("camp.toml").exists() || root.join("camp.db").exists() {
        bail!("a camp already exists at {}", root.display());
    }
    std::fs::create_dir_all(&root).with_context(|| format!("cannot create {}", root.display()))?;

    let name = camp_name(&root);
    std::fs::write(
        root.join("camp.toml"),
        format!("# Gas Camp configuration (spec §7.1)\n[camp]\nname = \"{name}\"\n"),
    )
    .with_context(|| format!("cannot write camp.toml in {}", root.display()))?;

    Ledger::open(&root.join("camp.db"))?;
    println!("initialized camp at {}", root.display());
    Ok(())
}

/// A repo-local `.camp` is named after the repo directory; an explicit camp
/// dir (e.g. ~/camps/dev) is named after itself.
fn camp_name(root: &Path) -> String {
    let own_name = root.file_name().and_then(|n| n.to_str());
    let dir_for_name = if own_name == Some(".camp") {
        root.parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
    } else {
        own_name
    };
    dir_for_name.unwrap_or("camp").to_owned()
}

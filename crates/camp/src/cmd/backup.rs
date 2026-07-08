use std::path::PathBuf;

use anyhow::{Context, Result};
use camp_core::ledger::Ledger;

use crate::campdir::CampDir;

/// `camp backup <DEST>`: write a consistent, integrity-checked copy of the
/// camp ledger to DEST via SQLite `VACUUM INTO`. DEST must not already
/// exist. Read-only on the source, so it is safe to run against a live camp.
pub fn run(camp: &CampDir, dest: PathBuf) -> Result<()> {
    let ledger = Ledger::open_read_only(&camp.db_path())?;
    ledger.backup_into(&dest).with_context(|| {
        format!(
            "backing up {} to {}",
            camp.db_path().display(),
            dest.display()
        )
    })?;
    println!("backup written to {} (integrity_check ok)", dest.display());
    Ok(())
}

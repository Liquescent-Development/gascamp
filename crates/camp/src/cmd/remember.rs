use anyhow::Result;

use crate::campdir::CampDir;

/// `camp remember "<fact>" [--rig r]`: persistent memory is a bead —
/// `bead.created` with `type='memory'`, title = the fact (spec §7.4). This
/// reuses the create path wholesale: same rig resolution, same per-rig id
/// allocation, same fold-written FTS row; prints the new bead id.
pub fn run(camp: &CampDir, fact: String, rig: Option<String>) -> Result<()> {
    crate::cmd::create::run(
        camp,
        fact,
        rig,
        None,
        Vec::new(),
        Vec::new(),
        Some("memory".to_owned()),
        None,
        // A memory is never a run member.
        None,
    )
}

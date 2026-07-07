use anyhow::Result;

use crate::campdir::CampDir;

/// `camp recall <query> [--limit N]`: `camp search` narrowed to memory
/// beads — the read half of the worker skill's recall-before /
/// remember-after contract (spec §7.4).
pub fn run(camp: &CampDir, query: &str, limit: usize) -> Result<()> {
    crate::cmd::search::run_filtered(camp, query, Some("memory"), limit)
}

use anyhow::Result;
use camp_core::ledger::Ledger;

use crate::campdir::CampDir;

/// `camp search <query> [--limit N]`: ranked full-text search over
/// everything, all time (spec §7.4). One line per hit:
/// `<bead_id>\t<kind>\t<snippet>`; no hits prints nothing and exits 0.
pub fn run(camp: &CampDir, query: &str, limit: usize) -> Result<()> {
    run_filtered(camp, query, None, limit)
}

/// Shared engine for `search` (unfiltered) and `recall` (memory only).
pub fn run_filtered(
    camp: &CampDir,
    query: &str,
    type_filter: Option<&str>,
    limit: usize,
) -> Result<()> {
    let ledger = Ledger::open(&camp.db_path())?;
    for hit in ledger.search(query, type_filter, limit)? {
        // Snippets can span the fold's title'\n'description boundary and
        // carry any whitespace the author typed; the output is one 3-column
        // TSV row per hit, so flatten line breaks AND tabs.
        let snippet = hit.snippet.replace(['\n', '\r', '\t'], " ");
        println!("{}\t{}\t{}", hit.bead_id, hit.kind, snippet.trim());
    }
    Ok(())
}

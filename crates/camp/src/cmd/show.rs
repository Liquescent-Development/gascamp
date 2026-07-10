use anyhow::{Result, anyhow};
use camp_core::event::Event;
use camp_core::ledger::Ledger;
use camp_core::readiness::BeadRow;

use crate::campdir::CampDir;

/// A bead's current state plus its full history — the single value both
/// renderings consume (DRY). `deliverable` is populated only for a shipped
/// bead (Task 2); it stays `None` otherwise.
pub(crate) struct BeadView {
    row: BeadRow,
    ready: bool,
    history: Vec<Event>,
    deliverable: Option<Deliverable>,
}

/// Shipped deliverable coordinates, promoted to first-class fields so no
/// one does git archaeology to find the result (design §6).
pub(crate) struct Deliverable {
    branch: String,
    commit: String,
    rig_path: String,
}

/// `camp show <bead> [--json]`: current state plus full event history — the
/// one sanctioned history read (spec §7.4). Read-only: `show` never writes.
pub fn run(
    camp: &CampDir,
    bead: String,
    json: bool,
    _wait: bool,
    _timeout: Option<u64>,
) -> Result<()> {
    let view = load_view(camp, &bead)?;
    if json {
        render_json(&view)
    } else {
        render_human(&view);
        Ok(())
    }
}

/// Read one bead read-only: row + readiness + history. Errors if unknown.
fn load_view(camp: &CampDir, bead: &str) -> Result<BeadView> {
    let ledger = Ledger::open_read_only(&camp.db_path())?;
    let row = ledger
        .get_bead(bead)?
        .ok_or_else(|| anyhow!("no such bead: {bead}"))?;
    let ready = ledger.is_ready(bead)?;
    let history = ledger.events_for_bead(bead)?;
    // Deliverable promotion is wired in Task 2; until then, always None.
    let deliverable = None;
    Ok(BeadView {
        row,
        ready,
        history,
        deliverable,
    })
}

/// The plain-text rendering — byte-for-byte the historical layout, plus the
/// promoted deliverable lines when present.
fn render_human(view: &BeadView) {
    let row = &view.row;
    println!("bead     {}", row.id);
    println!("rig      {}", row.rig);
    println!("type     {}", row.kind);
    println!("title    {}", row.title);
    println!(
        "status   {}{}",
        row.status,
        if view.ready { "  (ready)" } else { "" }
    );
    if let Some(a) = &row.assignee {
        println!("assignee {a}");
    }
    if let Some(c) = &row.claimed_by {
        println!("claimed  {c}");
    }
    if let Some(o) = &row.outcome {
        println!("outcome  {o}");
    }
    if let Some(wo) = &row.work_outcome {
        println!("work     {wo}");
    }
    if let Some(d) = &view.deliverable {
        println!("branch   {}", d.branch);
        println!(
            "commit   {}   (see: git -C {} show {})",
            d.commit, d.rig_path, d.commit
        );
    }
    if let Some(df) = &row.dispatch_failure {
        // Assessment finding A (PR #54): the marker alone hides the retry
        // semantics — campd's in-memory failed set suppresses re-dispatch
        // for its lifetime (fail-fast by design), so fixing the cause is
        // not enough; say so where the reason is read.
        println!("dispatch-failed  {df}");
        println!(
            "                 (campd retries once per restart — after fixing the cause, restart campd)"
        );
    }
    if !row.labels.is_empty() {
        println!("labels   {}", row.labels.join(", "));
    }
    println!("created  {}", row.created_ts);
    println!("updated  {}", row.updated_ts);
    println!();
    println!("history:");
    for e in &view.history {
        println!(
            "  {:>4}  {}  {:<14}  {}",
            e.seq,
            e.ts,
            e.kind.as_str(),
            e.data
        );
    }
}

/// The machine rendering — one JSON object: state fields + `history` array.
fn render_json(view: &BeadView) -> Result<()> {
    let row = &view.row;
    let mut obj = serde_json::json!({
        "bead": row.id,
        "rig": row.rig,
        "type": row.kind,
        "title": row.title,
        "status": row.status,
        "ready": view.ready,
        "assignee": row.assignee,
        "claimed_by": row.claimed_by,
        "outcome": row.outcome,
        "work_outcome": row.work_outcome,
        "dispatch_failure": row.dispatch_failure,
        "labels": row.labels,
        "created": row.created_ts,
        "updated": row.updated_ts,
        "history": view.history,
    });
    if let Some(d) = &view.deliverable {
        obj["branch"] = serde_json::json!(d.branch);
        obj["commit"] = serde_json::json!(d.commit);
    }
    println!("{}", serde_json::to_string_pretty(&obj)?);
    Ok(())
}

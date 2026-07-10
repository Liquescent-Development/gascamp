use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use camp_core::config::CampConfig;
use camp_core::event::{Event, EventType};
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

/// `camp show <bead> [--json] [--wait [--timeout SECONDS]]`: current state
/// plus full event history (spec §7.4). Read-only: `show` never writes and
/// never autostarts campd — `--wait` is a pure observer (design §7).
pub fn run(
    camp: &CampDir,
    bead: String,
    json: bool,
    wait: bool,
    timeout: Option<u64>,
) -> Result<()> {
    let view = if wait {
        wait_for_close(camp, &bead, timeout.map(Duration::from_secs))?
    } else {
        load_view(camp, &bead)?
    };
    if json {
        render_json(&view)
    } else {
        render_human(&view);
        Ok(())
    }
}

/// Block until `bead` reaches a `closed` status, then return its view.
///
/// Sleeps on a `notify` file-watch of the camp directory — WAL commits land
/// there, so every close wakes us; there is NO poll loop (invariant #1). The
/// watch is armed BEFORE the re-check, so a close landing between the first
/// read and arming cannot be missed (arm-before-check). Pure observer: no
/// ledger writes, no campd dependency — a worker's `camp close` writes the
/// terminal event to the ledger directly, and we observe that ground truth.
fn wait_for_close(camp: &CampDir, bead: &str, timeout: Option<Duration>) -> Result<BeadView> {
    let view = load_view(camp, bead)?; // also validates the bead exists
    if view.row.status == "closed" {
        return Ok(view);
    }
    let (tx, rx) = mpsc::channel::<()>();
    let mut watcher = notify::recommended_watcher(move |_res: notify::Result<notify::Event>| {
        // Any change under the camp dir is a wake; the reload reads ground
        // truth. One pending wake is enough — a failed send just means a
        // wake is already queued.
        let _ = tx.send(());
    })
    .map_err(|e| anyhow!("creating the ledger watcher: {e}"))?;
    notify::Watcher::watch(
        &mut watcher,
        &camp.root,
        notify::RecursiveMode::NonRecursive,
    )
    .map_err(|e| anyhow!("watching the camp directory {}: {e}", camp.root.display()))?;
    // Re-check AFTER arming (closes the arm-before-check race).
    let view = load_view(camp, bead)?;
    if view.row.status == "closed" {
        return Ok(view);
    }
    eprintln!("waiting for {bead} to close (Ctrl-C to stop)…");
    let deadline = timeout.map(|t| Instant::now() + t);
    loop {
        match deadline {
            None => match rx.recv() {
                // Unbounded: a pure blocking wait on the OS watch — no tick.
                Ok(()) => {}
                Err(_) => anyhow::bail!("ledger watcher disconnected while waiting for {bead}"),
            },
            Some(d) => {
                let now = Instant::now();
                if now >= d {
                    let view = load_view(camp, bead)?;
                    anyhow::bail!(
                        "timed out waiting for {bead} to close — still {} after the timeout",
                        view.row.status
                    );
                }
                match rx.recv_timeout(d - now) {
                    Ok(()) => {}
                    Err(mpsc::RecvTimeoutError::Timeout) => continue, // re-eval deadline → bail
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        anyhow::bail!("ledger watcher disconnected while waiting for {bead}")
                    }
                }
            }
        }
        let view = load_view(camp, bead)?;
        if view.row.status == "closed" {
            return Ok(view);
        }
        // Spurious/unrelated fs event (e.g. a -shm touch): keep waiting.
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
    let deliverable = if row.work_outcome.as_deref() == Some("shipped") {
        Some(build_deliverable(camp, &row, &history)?)
    } else {
        None
    };
    Ok(BeadView {
        row,
        ready,
        history,
        deliverable,
    })
}

/// Resolve a shipped bead's deliverable coordinates: branch + commit from
/// the last `bead.closed` event's data, and the rig path from config (the
/// same resolution `cmd/close.rs` uses). The commit lives on `camp/<bead>`
/// in the RIG repo — campd reaps the worktree on close (spec §12), so the
/// rig repo is the durable location the pointer names.
fn build_deliverable(camp: &CampDir, row: &BeadRow, history: &[Event]) -> Result<Deliverable> {
    let closed = history
        .iter()
        .rev()
        .find(|e| e.kind == EventType::BeadClosed)
        .ok_or_else(|| anyhow!("bead {} is shipped but has no bead.closed event", row.id))?;
    let field = |key: &str| -> Result<String> {
        closed
            .data
            .get(key)
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("shipped close for {} records no {key}", row.id))
    };
    let branch = field("work_branch")?;
    let commit = field("work_commit")?;
    let config = CampConfig::load(&camp.config_path())?;
    let rig_path = config.rig(&row.rig)?.path.display().to_string();
    Ok(Deliverable {
        branch,
        commit,
        rig_path,
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
        // Always present (null when not shipped, string when shipped) so
        // machine consumers get a uniform shape rather than a
        // sometimes-absent key (Task 1 reviewer note).
        "branch": null,
        "commit": null,
    });
    if let Some(d) = &view.deliverable {
        obj["branch"] = serde_json::json!(d.branch);
        obj["commit"] = serde_json::json!(d.commit);
    }
    println!("{}", serde_json::to_string_pretty(&obj)?);
    Ok(())
}

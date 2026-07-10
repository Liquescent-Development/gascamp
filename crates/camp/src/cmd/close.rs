use anyhow::{Context, Result, anyhow, bail};
use camp_core::config::CampConfig;
use camp_core::event::{EventInput, EventType};
use camp_core::ledger::Ledger;

use crate::campdir::CampDir;

/// `camp close <bead> --outcome pass|fail [--reason r] [--transient]
/// [--output-json <file|->] [--work-outcome shipped|no-op|blocked|abandoned]
/// [--work-commit SHA --work-branch BRANCH]`: close with an outcome.
/// `--transient` is the worker's retry classification (spec §8.2,
/// `failure_class:"transient"`); `--output-json` records structured step
/// output in the close event's `data.output` for `on_complete` fan-out;
/// `--work-outcome` records gc's WorkOutcome axis (Phase 3, #34) — a
/// separate, additive axis from the control outcome. A `shipped` close
/// must first pass `verify_shipped`, the mechanical git gate.
#[allow(clippy::too_many_arguments)]
pub fn run(
    camp: &CampDir,
    bead: String,
    outcome: String,
    reason: Option<String>,
    transient: bool,
    output_json: Option<String>,
    work_outcome: Option<String>,
    work_commit: Option<String>,
    work_branch: Option<String>,
) -> Result<()> {
    if transient && outcome != "fail" {
        // same rule the fold enforces; fail at the user's prompt
        bail!("--transient requires --outcome fail (it classifies a failure)");
    }
    match work_outcome.as_deref() {
        None => {
            if work_commit.is_some() || work_branch.is_some() {
                bail!("--work-commit/--work-branch require --work-outcome shipped");
            }
        }
        Some(wo @ ("shipped" | "no-op")) if outcome != "pass" => {
            bail!("--work-outcome {wo} requires --outcome pass");
        }
        Some(wo @ ("blocked" | "abandoned")) if outcome != "fail" => {
            bail!("--work-outcome {wo} requires --outcome fail (the work did not land)");
        }
        Some("shipped") => {
            let commit = work_commit.as_deref().ok_or_else(|| {
                anyhow!(
                    "--work-outcome shipped requires --work-commit (the commit that satisfies the bead)"
                )
            })?;
            let branch = work_branch.as_deref().ok_or_else(|| {
                anyhow!(
                    "--work-outcome shipped requires --work-branch (the branch the commit lives on)"
                )
            })?;
            verify_shipped(camp, &bead, commit, branch)?;
        }
        Some(_) => {
            if work_commit.is_some() || work_branch.is_some() {
                bail!("only --work-outcome shipped carries --work-commit/--work-branch");
            }
        }
    }
    let output = match output_json {
        None => None,
        Some(source) => {
            let raw = if source == "-" {
                let mut buf = String::new();
                std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)
                    .context("reading --output-json from stdin")?;
                buf
            } else {
                std::fs::read_to_string(&source)
                    .with_context(|| format!("reading --output-json file {source}"))?
            };
            let value: serde_json::Value = serde_json::from_str(&raw)
                .with_context(|| format!("--output-json {source} is not valid JSON"))?;
            Some(value)
        }
    };
    let mut ledger = Ledger::open(&camp.db_path())?;
    let mut data = serde_json::json!({ "outcome": outcome });
    if let Some(r) = reason {
        data["reason"] = serde_json::json!(r);
    }
    if transient {
        data["failure_class"] = serde_json::json!("transient");
    }
    if let Some(output) = output {
        data["output"] = output;
    }
    // Only when present (obligation iv): a plain close keeps the v1 payload
    // shape, byte for byte.
    if let Some(wo) = &work_outcome {
        data["work_outcome"] = serde_json::json!(wo);
    }
    if let Some(c) = &work_commit {
        data["work_commit"] = serde_json::json!(c);
    }
    if let Some(b) = &work_branch {
        data["work_branch"] = serde_json::json!(b);
    }
    let seq = ledger.append(EventInput {
        kind: EventType::BeadClosed,
        rig: None,
        actor: "cli".into(),
        bead: Some(bead.clone()),
        data,
    })?;
    crate::daemon::socket::poke_best_effort(camp, seq);
    println!(
        "closed {bead} ({outcome}{})",
        work_outcome
            .as_deref()
            .map(|wo| format!(", {wo}"))
            .unwrap_or_default()
    );
    Ok(())
}

/// The shipped gate (dispatch-lifecycle Phase 3, Q4 — #34): "landed" in v1
/// is a LOCAL fact, mechanically checkable, four git facts strong (PR #54
/// review finding 1 hardened the last three against self-certification):
/// the named branch is a REAL local branch (a bare commit-ish would pass
/// ancestry checks — a commit is its own ancestor — and a SHA "branch" is
/// git-gc-able, voiding branch-outlives-reap); the commit is reachable on
/// that branch (gc's work-record gate rule); the commit DESCENDS from the
/// dispatch-time base recorded on the claiming session's woke event; and
/// the commit is NOT the base itself — shipped asserts at least one commit
/// of new work (`rev-parse HEAD` on an unchanged tree is exactly #34's
/// self-certification, on a based rig). All git runs against the rig path:
/// worktrees share the object store, so bead-branch refs resolve from the
/// rig. gc ships its gate warn-only by default; camp enforces always
/// (invariant 5 — an unverifiable `shipped` is rejected, never recorded).
fn verify_shipped(camp: &CampDir, bead: &str, commit: &str, branch: &str) -> Result<()> {
    for (flag, value) in [("--work-commit", commit), ("--work-branch", branch)] {
        if value.starts_with('-') {
            bail!("{flag} value {value:?} must not begin with '-'");
        }
    }
    let ledger = Ledger::open(&camp.db_path())?;
    let row = ledger
        .get_bead(bead)?
        .ok_or_else(|| anyhow!("no such bead: {bead}"))?;
    let session = row.claimed_by.as_deref().ok_or_else(|| {
        anyhow!(
            "shipped requires a claiming session with a recorded dispatch-time base; \
             bead {bead} has no claiming session — record --work-outcome blocked (or no-op) instead"
        )
    })?;
    let base = ledger
        .session_by_name(session)?
        .and_then(|s| s.base)
        .ok_or_else(|| {
            anyhow!(
                "session {session:?} has no dispatch-time base recorded (the rig had no base \
                 commit when this work was dispatched) — the work cannot have landed; close it \
                 --work-outcome blocked with the reason"
            )
        })?;
    let config = CampConfig::load(&camp.config_path())?;
    let rig_path = &config.rig(&row.rig)?.path;
    drop(ledger); // reopened by the caller for the append
    let git = |args: &[&str]| -> Result<std::process::Output> {
        std::process::Command::new("git")
            .arg("-C")
            .arg(rig_path)
            .args(args)
            .output()
            .with_context(|| format!("running git {args:?}"))
    };
    // The branch must be a real local ref — checked FIRST, so no later
    // ancestry test can be fed a commit-ish "branch" (its own ancestor).
    let branch_ref = format!("refs/heads/{branch}");
    if !git(&["rev-parse", "--verify", "--quiet", &branch_ref])?
        .status
        .success()
    {
        bail!(
            "work_branch {branch:?} is not a local branch in rig {} — shipped work \
             lives on a real branch that outlives the worktree",
            row.rig
        );
    }
    // Resolve the commit to its full sha (an abbreviated base must not
    // slip past the new-work equality check below).
    let resolved = git(&[
        "rev-parse",
        "--verify",
        "--quiet",
        &format!("{commit}^{{commit}}"),
    ])?;
    if !resolved.status.success() {
        bail!(
            "work_commit {commit:?} does not name a commit in rig {}",
            row.rig
        );
    }
    let commit = String::from_utf8_lossy(&resolved.stdout).trim().to_owned();
    if !git(&["merge-base", "--is-ancestor", &commit, &branch_ref])?
        .status
        .success()
    {
        bail!(
            "work_commit {commit} is not reachable on work_branch {branch:?} in rig {} — \
             shipped must name the commit as it exists on its branch",
            row.rig
        );
    }
    if commit == base {
        bail!(
            "work_commit {commit} IS the dispatch-time base — shipped requires at least \
             one commit of new work; record --work-outcome no-op (nothing was needed) or \
             blocked instead"
        );
    }
    if !git(&["merge-base", "--is-ancestor", &base, &commit])?
        .status
        .success()
    {
        bail!(
            "work_commit {commit} does not descend from the dispatch-time base {base} — \
             the work has no path from the state it was dispatched on; close it \
             --work-outcome blocked instead"
        );
    }
    Ok(())
}

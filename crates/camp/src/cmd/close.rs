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
    crate::daemon::socket::poke_best_effort(&camp.socket_path(), seq);
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
/// is a LOCAL fact, mechanically checkable — the commit is reachable on
/// its branch (gc's work-record gate rule, verbatim) AND descends from the
/// dispatch-time base recorded on the claiming session's woke event. All
/// git runs against the rig path: worktrees share the object store, so
/// bead-branch refs resolve from the rig. gc ships this gate warn-only by
/// default; camp enforces always (invariant 5 — an unverifiable `shipped`
/// is rejected, never recorded).
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
    let git = |args: &[&str]| -> Result<bool> {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(rig_path)
            .args(args)
            .output()
            .with_context(|| format!("running git {args:?}"))?;
        Ok(out.status.success())
    };
    if !git(&["merge-base", "--is-ancestor", commit, branch])? {
        bail!(
            "work_commit {commit} is not reachable on work_branch {branch:?} in rig {} — \
             shipped must name the commit as it exists on its branch",
            row.rig
        );
    }
    if !git(&["merge-base", &base, commit])? {
        bail!(
            "work_commit {commit} shares no history with the dispatch-time base {base} \
             (no merge-base) — the branch has no path to the rig's integration branch; \
             close it --work-outcome blocked instead"
        );
    }
    Ok(())
}

use anyhow::{Context, Result, anyhow, bail};
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
/// separate, additive axis from the control outcome.
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
            verify_shipped(camp, &bead, commit, branch)?; // Task 7 (stub: Ok(()) in this task)
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

/// The mechanical `shipped` gate (design §4.3): the commit must be
/// reachable on the named branch AND descend from the session's
/// dispatch-time base. Task 7 implements it; until then every shipped
/// close that reaches this point is accepted unverified.
fn verify_shipped(_camp: &CampDir, _bead: &str, _commit: &str, _branch: &str) -> Result<()> {
    // Task 7
    Ok(())
}

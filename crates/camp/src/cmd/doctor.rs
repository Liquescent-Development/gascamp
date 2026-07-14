use anyhow::{Result, bail};
use camp_core::ledger::Ledger;

use crate::campdir::CampDir;

/// `camp doctor --refold [--repair]`: verify (or rebuild) the fold property —
/// state tables ≡ fold of the event log (spec §13.5).
pub fn run(camp: &CampDir, repair: bool) -> Result<()> {
    let mut ledger = Ledger::open(&camp.db_path())?;
    let report = if repair {
        ledger.refold_repair()?
    } else {
        ledger.refold_check()?
    };
    if report.drift.is_empty() {
        println!(
            "refold: replayed {} events; 0 drift rows",
            report.events_replayed
        );
        Ok(())
    } else {
        for entry in &report.drift {
            println!("drift in {}: {}", entry.table, entry.detail);
        }
        bail!(
            "refold drift detected: {} rows (camp doctor --refold --repair rebuilds state from the event log)",
            report.drift.len()
        );
    }
}

/// `camp doctor --formula <path>`: validate one formula file against the
/// camp subset (spec §8.2). Exit 0 = valid camp formula (and therefore a
/// valid Gas City formula-v2 file, repo invariant 6); exit 1 = every
/// violation printed, not just the first.
pub fn run_formula(
    camp: &crate::campdir::CampDir,
    path: &std::path::Path,
    json: bool,
    compiled_shape: bool,
) -> Result<()> {
    let config = camp_core::config::CampConfig::load(&camp.config_path())?;
    let layers = camp_core::formula::FormulaLayers::from_config(&config, &camp.root)?;
    let verdict =
        camp_core::formula::compile(&layers, &config, path, &std::collections::BTreeMap::new());

    // --compiled: camp's COMPILED steps in the differential gate's normalized
    // shape. It is the SAME projection `factshim --authored-json` emits for gc, so
    // the oracle can diff them field for field.
    if compiled_shape {
        let steps = match &verdict {
            Ok(c) => c
                .formula
                .steps
                .iter()
                .map(|s| {
                    let description = s.description.clone().unwrap_or_default();
                    serde_json::json!({
                        "id": s.id,
                        "kind": s.metadata.get("gc.kind"),
                        "title": s.title,
                        "description_sha256": sha256_hex(&description),
                        // The >4096 pointer prompt embeds an ABSOLUTE resolved
                        // path — environment-dependent by construction, since gc
                        // resolves inside the corpus checkout and camp inside its
                        // import tree. Both sides blank that ONE line and hash the
                        // rest, which is the part a mis-transcription corrupts.
                        "description_sha256_norm": sha256_hex(&normalize_description(&description)),
                        "assignee": s.assignee.clone().unwrap_or_default(),
                        "metadata": s.metadata,
                        "needs": s.needs,
                    })
                })
                .collect::<Vec<_>>(),
            Err(_) => Vec::new(),
        };
        println!(
            "{}",
            serde_json::json!({
                "formula": camp_core::formula::formula_stem(path),
                "ok": verdict.is_ok(),
                "steps": steps,
            })
        );
        return Ok(());
    }

    if json {
        // The VERDICT is the output, and it EXITS 0 even when the formula does
        // not load: the §10 gate reads 100 verdicts and counts them, and a
        // non-zero exit on a formula camp deliberately refuses would make the
        // gate's own success indistinguishable from a crash.
        let out = match &verdict {
            Ok(c) => serde_json::json!({
                "path": path,
                "formula": c.formula.name,
                "ok": true,
                "runnable": c.is_runnable(),
                "ignored_keys": c.ignored_keys,
                "refusals": [],
                "not_runnable": c.not_runnable.as_ref().map(|r| serde_json::json!({
                    "key": r.key, "reason": r.reason,
                })),
                "steps": c.formula.steps.iter().map(|s| &s.id).collect::<Vec<_>>(),
            }),
            Err(e) => serde_json::json!({
                "path": path,
                "formula": camp_core::formula::formula_stem(path),
                "ok": false,
                "runnable": false,
                "ignored_keys": [],
                "violations": e.violations.iter().map(|v| serde_json::json!({
                    "construct": v.construct, "message": v.message,
                })).collect::<Vec<_>>(),
                // A refusal NAMES ITS KEY — which, for a scope-check hiding in
                // step metadata, is not even the key that carried it (§4 trap 2).
                "refusals": e.refusals.iter().map(|r| serde_json::json!({
                    "construct": r.construct, "key": r.key, "reason": r.reason, "step": r.step,
                })).collect::<Vec<_>>(),
                "not_runnable": serde_json::Value::Null,
            }),
        };
        println!("{out}");
        return Ok(());
    }

    match verdict {
        Ok(c) => {
            println!(
                "formula ok: {} ({} step(s)){}",
                c.formula.name,
                c.formula.steps.len(),
                if c.is_runnable() {
                    String::new()
                } else {
                    format!(
                        " — COMPILES BUT IS NOT RUNNABLE: {}",
                        c.not_runnable.as_ref().map_or("", |r| r.reason.as_str())
                    )
                }
            );
            for warning in &c.ignored_keys {
                println!("warning: {warning}");
            }
            Ok(())
        }
        Err(err) => {
            // BOTH buckets. A refusal is not a violation, and a `phase`-refused
            // formula used to print nothing at all — the operator was told the
            // load failed and never told why.
            for violation in &err.violations {
                println!("{violation}");
            }
            for refusal in &err.refusals {
                println!("{refusal}");
            }
            bail!(
                "{}: {} violation(s), {} refusal(s) — camp reads Gas City formula v2 \
                 permissively but refuses constructs it does not implement, by name \
                 (compat §4)",
                err.path.display(),
                err.violations.len(),
                err.refusals.len()
            );
        }
    }
}

/// `camp doctor --drain-reservations [--release-orphans]` — THE OPERATOR ESCAPE.
///
/// A member held by an anchor that will never gather it is a member no drain can
/// ever take. `reconcile` sweeps orphans on every campd start; this is the manual
/// lever for when campd is not the one running, and the visibility for when an
/// operator wants to know who holds what.
pub fn run_drain_reservations(camp: &CampDir, release: bool) -> Result<()> {
    let mut ledger = Ledger::open(&camp.db_path())?;
    let orphans = ledger.orphaned_reservations()?;

    if !release {
        if orphans.is_empty() {
            println!("no orphaned drain reservations");
        } else {
            for (member, anchor) in &orphans {
                println!(
                    "ORPHAN {member} held by {anchor} (that anchor is closed or gone — it will \
                     never gather this member)"
                );
            }
            println!(
                "\n{} orphaned reservation(s). `camp doctor --drain-reservations \
                 --release-orphans` releases them.",
                orphans.len()
            );
        }
        return Ok(());
    }

    let released = crate::daemon::dispatch::release_orphaned_reservations(&mut ledger)?;
    println!("released {} orphaned drain reservation(s)", released.len());
    for (member, anchor) in &released {
        println!("  {member} (was held by {anchor})");
    }
    Ok(())
}

fn sha256_hex(s: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    format!("{:x}", h.finalize())
}

/// Blank the ONE environment-dependent line of gc's >4096 pointer prompt. See
/// `factshim.go::normalizeDescription` — the two must agree exactly.
fn normalize_description(d: &str) -> String {
    // `split('\n')`, NOT `lines()`: `lines()` DROPS a trailing newline, so every
    // description would hash differently from gc's — which uses Go's
    // `strings.Split`/`Join`, and that round-trips the trailing newline exactly.
    // The differential gate caught this as 294 false divergences before it caught
    // any real one; a normalizer that is not byte-identical on both sides is not a
    // normalizer.
    d.split('\n')
        .map(|l| {
            if l.starts_with("- Resolved prompt file: ") {
                "- Resolved prompt file: <normalized>"
            } else {
                l
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

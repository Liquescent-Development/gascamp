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

/// `camp doctor --orphan-runs [--sweep-orphan-runs]` — the run-dir leftovers of
/// a crash (#124).
///
/// cook writes `runs/<id>/` BEFORE its one ledger batch (cook.rs's header: the
/// safe direction — a DB-first cook could commit a run whose pinned formula
/// never reached disk). A `kill -9` in that window leaves a dir no ledger row
/// names. Recovery is already idempotent; the DIRECTORY is what leaks, and it
/// leaks again on every crash.
///
/// LISTING IS READ-ONLY and always available. The sweep is not, because of:
///
/// THE RACE — a run dir with no `run.cooked` is EXACTLY what a healthy
/// in-flight cook looks like between its dir write and its commit. A naive "no
/// run.cooked → delete" deletes live run state. Three things make this safe,
/// and all three are load-bearing:
///
///  1. **campd must be DOWN** (probed here, on the socket — spec §5: liveness
///     IS the socket accepting). campd is where orders, bonds and drains cook,
///     so this removes the overwhelming majority of possible in-flight cooks,
///     and with it the window where campd commits a `run.cooked` between our
///     ledger read and our `rm`.
///  2. **A grace window** (`ORPHAN_RUN_SWEEP_GRACE`), because campd-down is not
///     enough on its own: `camp sling` cooks with campd stopped, so a second
///     terminal can be mid-cook right now. Its dir's mtime is milliseconds old;
///     the window is ten minutes.
///  3. **Enumerate dirs before reading the ledger** (in `orphaned_run_dirs`),
///     so a cook that commits mid-scan is seen as cooked, not as an orphan.
///
/// Why that is sufficient rather than merely comforting: cook NEVER adopts an
/// existing run dir — on an id collision it regenerates the id and creates a
/// fresh one (cook.rs). So no future cook can ever retroactively claim a dir we
/// have already enumerated. The only process that can legitimately commit a
/// `run.cooked` for dir X is the one that created X, and (1) and (2) between
/// them exclude exactly that process.
///
/// Refusing costs an operator one `camp service stop`. Being wrong costs a run.
pub fn run_orphan_runs(camp: &CampDir, sweep: bool) -> Result<()> {
    let ledger = Ledger::open(&camp.db_path())?;

    if !sweep {
        let orphans = ledger.orphaned_run_dirs(&camp.root)?;
        if orphans.is_empty() {
            println!("no orphaned run directories");
            return Ok(());
        }
        for orphan in &orphans {
            println!(
                "ORPHAN {} ({} — no run.cooked event names this run){}",
                orphan.path.display(),
                describe_idle(orphan),
                if orphan.sweepable() {
                    ""
                } else {
                    " — TOO YOUNG TO SWEEP: this is also what a healthy in-flight cook looks like"
                }
            );
        }
        let sweepable = orphans.iter().filter(|o| o.sweepable()).count();
        println!(
            "\n{} orphaned run directory(s), {sweepable} old enough to sweep. \
             `camp doctor --orphan-runs --sweep-orphan-runs` removes them (stop campd first).",
            orphans.len()
        );
        return Ok(());
    }

    // Defense (1). `request_if_up` judges liveness on the connection itself —
    // no bare pre-probe (the PR #51 finding 1 law) — and a campd that accepts
    // but misbehaves still surfaces as Err, which is also a refusal to sweep.
    if crate::daemon::socket::request_if_up(camp, &crate::daemon::socket::Request::Status)?
        .is_some()
    {
        bail!(
            "campd is running in this camp — refusing to sweep run directories.\n\
             A run dir with no run.cooked event is exactly what a cook looks like \
             mid-flight, and campd is the thing that cooks. Stop it first \
             (`camp service stop`), then re-run. `camp doctor --orphan-runs` lists \
             them read-only at any time."
        );
    }

    let swept = ledger.sweep_orphan_run_dirs(&camp.root)?;
    println!("swept {} orphaned run directory(s)", swept.len());
    for orphan in &swept {
        println!("  {} ({})", orphan.path.display(), describe_idle(orphan));
    }
    // A dir the sweep DECLINED is the interesting half: say so rather than
    // leave the operator wondering why `swept 0` followed a listing that named
    // three orphans.
    let spared: Vec<_> = ledger
        .orphaned_run_dirs(&camp.root)?
        .into_iter()
        .filter(|o| !o.sweepable())
        .collect();
    if !spared.is_empty() {
        println!(
            "\n{} orphaned run directory(s) left alone — TOO YOUNG TO SWEEP (a cook \
             could still be writing there; re-run once they have been idle {}s):",
            spared.len(),
            camp_core::formula::runtime::ORPHAN_RUN_SWEEP_GRACE.as_secs()
        );
        for orphan in &spared {
            println!("  {} ({})", orphan.path.display(), describe_idle(orphan));
        }
    }
    Ok(())
}

fn describe_idle(orphan: &camp_core::formula::runtime::OrphanRunDir) -> String {
    match orphan.idle {
        Some(idle) => format!("idle {}s", idle.as_secs()),
        // Not a detail: an age we cannot read is why this dir will never be
        // swept, and the operator is owed the reason.
        None => "idle UNKNOWN (unreadable or future mtime)".to_owned(),
    }
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[allow(non_snake_case)]
mod tests {
    use super::*;
    use crate::daemon::socket::{Response, fake_campd};
    use camp_core::ledger::StatusSummary;

    /// A sweepable orphan (well past the grace window) in a camp with a real
    /// ledger — everything the sweep needs except permission.
    fn camp_with_a_sweepable_orphan() -> (tempfile::TempDir, CampDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let camp = CampDir {
            root: dir.path().to_path_buf(),
        };
        Ledger::open(&camp.db_path()).unwrap();
        let orphan = camp.runs_path().join("20260705T211403-orph01");
        std::fs::create_dir_all(&orphan).unwrap();
        let past = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        std::fs::File::open(&orphan)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(past))
            .unwrap();
        (dir, camp, orphan)
    }

    /// One answer from a live campd. `fake_campd::serve` serves exactly as many
    /// connections as it has responses and then DROPS its listener — so a fake
    /// built with an empty response list is a campd that is already gone, and a
    /// test using one proves nothing about a live daemon. (It proved nothing
    /// here, until a mutation said so.)
    fn a_live_campd_status() -> Response {
        Response::Status {
            ok: true,
            summary: StatusSummary {
                live_sessions: Vec::new(),
                ready: 0,
                open: 0,
                stuck: 0,
                unread_mail: 0,
            },
            red: 0,
            campd_pid: 4242,
        }
    }

    /// Race defense (1). campd is the thing that cooks; a run dir with no
    /// `run.cooked` is what a cook looks like MID-FLIGHT. With campd up, the
    /// sweep cannot tell a crash leftover from work in progress, so it refuses
    /// — loudly, naming the remedy — instead of guessing.
    ///
    /// Mutation caught: delete the `request_if_up(..).is_some()` bail → the
    /// sweep proceeds against a LIVE campd and the dir is deleted → RED.
    #[test]
    fn the_sweep_REFUSES_while_campd_is_LIVE_and_deletes_nothing() {
        let (_dir, camp, orphan) = camp_with_a_sweepable_orphan();
        let campd = fake_campd::serve(&camp, vec![a_live_campd_status()]);

        let err = run_orphan_runs(&camp, true).unwrap_err();
        assert!(
            err.to_string().contains("campd is running"),
            "the refusal must name its reason: {err}"
        );
        assert!(
            orphan.exists(),
            "a live campd could be mid-cook: refuse, never delete"
        );
        assert_eq!(campd.served(), 1, "the refusal really asked the socket");
    }

    /// …and the READ-ONLY listing is never gated on campd: it deletes nothing,
    /// so there is nothing to be unsafe about. An operator must be able to see
    /// what is on their disk without stopping their daemon.
    ///
    /// Mutation caught: hoist the campd probe above the `if !sweep` early
    /// return → listing starts refusing → RED.
    #[test]
    fn LISTING_is_allowed_while_campd_is_live_and_still_deletes_nothing() {
        let (_dir, camp, orphan) = camp_with_a_sweepable_orphan();
        let campd = fake_campd::serve(&camp, vec![a_live_campd_status()]);

        run_orphan_runs(&camp, false).unwrap();
        assert!(orphan.exists(), "listing is read-only");
        assert_eq!(
            campd.served(),
            0,
            "listing does not even ASK campd: it deletes nothing, so there is \
             nothing to gate on"
        );
    }

    /// With campd DOWN the sweep proceeds — the refusal above is a real gate,
    /// not a permanent "no". (`fake_campd` is never started here, so the
    /// socket refuses the connect: exactly a stopped campd.)
    #[test]
    fn the_sweep_PROCEEDS_with_campd_down() {
        let (_dir, camp, orphan) = camp_with_a_sweepable_orphan();
        run_orphan_runs(&camp, true).unwrap();
        assert!(
            !orphan.exists(),
            "campd is down and the dir is idle: sweep it"
        );
    }
}

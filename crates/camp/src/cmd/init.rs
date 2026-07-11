use std::path::Path;

use anyhow::{Context, Result, bail};
use camp_core::ledger::Ledger;

use crate::service::{self, Decision, ServiceChoice, SystemProbe, SystemRunner};

/// Create a new camp: `<cwd>/.camp` by default, `--camp DIR` to choose. Then
/// (design §6) put its campd under the host's service manager where one
/// exists — `--service` forces it, `--no-service` skips it.
pub fn run(camp_flag: Option<&Path>, choice: ServiceChoice) -> Result<()> {
    let root = match camp_flag {
        Some(dir) => dir.to_path_buf(),
        None => std::env::current_dir()
            .context("cannot determine current directory")?
            .join(".camp"),
    };
    if root.join("camp.toml").exists() || root.join("camp.db").exists() {
        bail!("a camp already exists at {}", root.display());
    }
    std::fs::create_dir_all(&root).with_context(|| format!("cannot create {}", root.display()))?;

    let name = camp_name(&root);
    std::fs::write(
        root.join("camp.toml"),
        format!("# Gas Camp configuration (spec §7.1)\n[camp]\nname = \"{name}\"\n"),
    )
    .with_context(|| format!("cannot write camp.toml in {}", root.display()))?;

    Ledger::open(&root.join("camp.db"))?;

    // When the camp lives inside a git repo, keep its live runtime state
    // (ledger, socket, logs) out of git; `camp.toml` stays tracked (issue #35).
    crate::gitignore::ensure_camp_runtime_ignored(&root)?;

    println!("initialized camp at {}", root.display());

    // Design §6: detect a usable HOST service manager and act on the answer.
    // A container is not a failure — it is a different supervisor — so the
    // absent case is a VISIBLE hand-off on stderr, never a silent fallback.
    let runner = SystemRunner;
    let probe = SystemProbe::new(&runner);
    match service::decide(choice, service::detect(&probe)) {
        Decision::Install(manager) => {
            // The camp is already on disk by now. If the service install fails
            // (a path no unit could name, a manager that refused to bootstrap),
            // the operator must not be left reading a bare service error and
            // guessing whether `camp init` did anything — the same statement
            // `FailNoManager` below makes carefully. The install itself rolls
            // its OWN half back (no orphan unit file), so the camp really is
            // the only thing that survives.
            let installed = service::supervisor_for(manager, &probe, &runner)
                .and_then(|supervisor| {
                    crate::cmd::service::install(
                        supervisor.as_ref(),
                        &root,
                        &crate::cmd::service::camp_binary()?,
                    )
                })
                .with_context(|| {
                    // Deliberately does NOT assert that no unit exists: `install`
                    // also fails when a unit is ALREADY there (a stale one left
                    // by a previous camp at this path), and claiming "NO unit was
                    // installed" would then be false, with a suggested retry that
                    // fails the same way (m2). State only what is certainly true
                    // — the camp exists, campd is not supervised — and let the
                    // cause below say which it was.
                    format!(
                        "The camp at {} WAS created, but campd was NOT put under the host \
                         service manager (the cause is below) — the camp is usable: \
                         `camp service status` shows where it stands, `camp service install` \
                         retries, and `camp daemon --camp {}` runs campd yourself",
                        root.display(),
                        root.display()
                    )
                })?;
            print!("{installed}");
        }
        Decision::SkipByFlag => println!(
            "service: skipped (--no-service) — run `camp daemon --camp {}` under your supervisor",
            root.display()
        ),
        Decision::SkipNoManager => eprintln!(
            "camp: no host service manager detected (container/CI?) — run \
             `camp daemon --camp {}` under your supervisor (e.g. the container runtime)",
            root.display()
        ),
        Decision::FailNoManager => bail!(
            "--service: no host service manager detected (macOS launchd, or a reachable \
             systemd --user). The camp at {} was created, but NO unit was installed — run \
             `camp daemon --camp {}` under your supervisor instead.",
            root.display(),
            root.display()
        ),
    }
    Ok(())
}

/// A repo-local `.camp` is named after the repo directory; an explicit camp
/// dir (e.g. ~/camps/dev) is named after itself.
fn camp_name(root: &Path) -> String {
    let own_name = root.file_name().and_then(|n| n.to_str());
    let dir_for_name = if own_name == Some(".camp") {
        root.parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
    } else {
        own_name
    };
    dir_for_name.unwrap_or("camp").to_owned()
}

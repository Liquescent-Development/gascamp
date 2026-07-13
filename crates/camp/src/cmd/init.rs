use std::path::Path;

use anyhow::{Context, Result, bail};
use camp_core::ledger::Ledger;

use crate::service::{self, Decision, ServiceChoice, SystemProbe, SystemRunner};

/// The default starter-pack source (component decision 12): the gascamp
/// `packs/starter` on `main`. Pinned by `DEFAULT_STARTER_VERSION` to a sha
/// that carries the rewritten directory-shaped starter (Task 20). The sha
/// is finalized to the starter-rewrite commit when this stream merges; tests
/// never fetch it (they import a LOCAL `packs/starter` path).
const DEFAULT_STARTER_SOURCE: &str =
    "https://github.com/Liquescent-Development/gascamp/tree/main/packs/starter";
const DEFAULT_STARTER_VERSION: &str = "sha:0000000000000000000000000000000000000000";

/// What `camp init` decided to do about the starter pack (component §8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportDecision {
    /// TTY + no flag: prompt the operator (default yes).
    Prompt,
    /// `--import <src>` (or a prompted yes): install this source.
    Install(String),
    /// `--no-import`: skip.
    Skip,
    /// Not a TTY + no flag: hand off with the exact command on stderr.
    HandOff,
}

/// Pure decision over (is_tty, --import, --no-import) — component §8 table.
/// Never fetches; the default source is only reached on a prompted yes.
pub fn decide_import(is_tty: bool, import: Option<&str>, no_import: bool) -> ImportDecision {
    if let Some(src) = import {
        return ImportDecision::Install(src.to_owned());
    }
    if no_import {
        return ImportDecision::Skip;
    }
    if is_tty {
        ImportDecision::Prompt
    } else {
        ImportDecision::HandOff
    }
}

/// Create a new camp: `<cwd>/.camp` by default, `--camp DIR` to choose. Then
/// (design §6) put its campd under the host's service manager where one
/// exists — `--service` forces it, `--no-service` skips it.
///
/// `exists_ok` turns the "already a camp here" case from a hard error into a
/// no-op success. It exists for supervised entrypoints that re-run init on
/// every start (contrib/docker/): a restarted container with a persistent camp
/// volume MUST come back up, and a crash-loop over an error that says "yes,
/// the camp you asked for is right there" would be a lie about a failure.
/// It is a no-op, never a repair: an existing camp is returned as it is, and
/// no unit is installed for it (a camp created before this had a service
/// manager gets one from `camp service install` — an explicit act).
///
/// That is why `--exists-ok` returns BEFORE the service decision below, and
/// why it may: clap rejects `--service --exists-ok` as the contradiction it is
/// (`conflicts_with = "service"`), so the short-circuit can never swallow an
/// explicit request to install a unit. Honouring `--service` here instead
/// would make `camp init` REPAIR an existing camp's service state — exactly
/// the auto-migration feature design §11 rules out. The idempotent
/// provisioning path is `camp init --exists-ok && camp service install`: two
/// verbs, each of which means what it says.
pub fn run(
    camp_flag: Option<&Path>,
    choice: ServiceChoice,
    exists_ok: bool,
    import: Option<&str>,
    no_import: bool,
) -> Result<()> {
    let root = match camp_flag {
        Some(dir) => dir.to_path_buf(),
        None => std::env::current_dir()
            .context("cannot determine current directory")?
            .join(".camp"),
    };
    let already_exists = root.join("camp.toml").exists() || root.join("camp.db").exists();
    if already_exists {
        if exists_ok {
            println!("camp already exists at {} (--exists-ok)", root.display());
        } else {
            bail!("a camp already exists at {}", root.display());
        }
    } else {
        std::fs::create_dir_all(&root)
            .with_context(|| format!("cannot create {}", root.display()))?;
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
    }

    // Starter-pack decision (component §8). Composes with --exists-ok: an
    // existing camp can still import on re-run. Never fetches the default
    // source in a test (tests import a LOCAL path or assert the pure decision).
    let is_tty = std::io::IsTerminal::is_terminal(&std::io::stdin());
    let decision = decide_import(is_tty, import, no_import);
    match decision {
        ImportDecision::Prompt => {
            print!("Install the starter pack? [Y/n] ");
            use std::io::Write;
            std::io::stdout().flush().ok();
            let mut line = String::new();
            std::io::stdin().read_line(&mut line).ok();
            if line.trim().is_empty() || line.trim().eq_ignore_ascii_case("y") {
                install_default_starter(&root)?;
            }
        }
        ImportDecision::Install(src) => {
            if let Err(e) = crate::cmd::import::run_add(&root, &src, None, None) {
                bail!(
                    "The camp at {} WAS created, but the starter pack was NOT installed ({e:#}); \
                     the camp is usable — run `camp import add <source> --name <binding>` yourself",
                    root.display()
                );
            }
        }
        ImportDecision::Skip => {}
        ImportDecision::HandOff => eprintln!(
            "camp: not a TTY and no --import given; install a pack with \
             `camp import add <source> --name <binding>` (e.g. `camp import add \
             {DEFAULT_STARTER_SOURCE} --name starter`)"
        ),
    }

    // Service decision is a fresh-create concern only: an existing camp keeps
    // its unit state (design §11 — no auto-migration). `--exists-ok` skips it.
    if already_exists {
        return Ok(());
    }

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

/// Install the default starter pack (a prompted yes). Uses the pinned
/// `DEFAULT_STARTER_VERSION` so the materialization is reproducible. A fetch
/// failure exits non-zero ("camp WAS created, pack was NOT installed").
fn install_default_starter(root: &Path) -> Result<()> {
    if let Err(e) =
        crate::cmd::import::run_add(root, DEFAULT_STARTER_SOURCE, Some("starter"), Some(DEFAULT_STARTER_VERSION))
    {
        bail!(
            "The camp at {} WAS created, but the starter pack was NOT installed ({e:#}); \
             the camp is usable — run `camp import add {DEFAULT_STARTER_SOURCE} --name starter` yourself",
            root.display()
        );
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn decide_import_covers_the_matrix() {
        assert!(matches!(decide_import(true, None, false), ImportDecision::Prompt));
        assert!(matches!(decide_import(true, Some("file:///x"), false), ImportDecision::Install(s) if s == "file:///x"));
        assert!(matches!(decide_import(true, None, true), ImportDecision::Skip));
        assert!(matches!(decide_import(false, None, false), ImportDecision::HandOff));
        assert!(matches!(decide_import(false, Some("file:///x"), false), ImportDecision::Install(_)));
    }
}

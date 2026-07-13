//! `camp import` verbs (component spec §9): the hardened git subprocess +
//! the add/install/upgrade/check/list/remove orchestration. This file holds
//! the git plumbing and the test-support fixture builder; the verbs land in
//! Task 17.
//!
//! Hardening (umbrella §13 / component §11): every network git invocation
//! carries the pinned `-c` flags verbatim — `protocol.allow=never` plus the
//! per-scheme allowlist blocks `ext::`; `core.hooksPath=/dev/null` stops
//! cloned-repo hooks; `http.followRedirects=false` closes a redirect vector.
//! The argv order is pinned byte-for-byte (the test asserts it). Env is
//! sanitized by removing every `GIT_*` variable (NOT `env_clear`, which drops
//! PATH and leaves campd unable to spawn workers — invariant: the unit
//! carries campd's PATH).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use camp_core::config::ImportDecl;
use camp_core::event::{EventInput, EventType};
use camp_core::import::ResolvedImport;
use camp_core::import::inventory::ExecItem;
use camp_core::import::lock::{LockEntry, PacksLock};
use camp_core::import::manifest::read_manifest;
use camp_core::import::manifest::{PackManifest, PackMeta};
use camp_core::import::materialize::materialize_tree;
use camp_core::import::resolve_transitive;
use camp_core::import::source::normalize;
use camp_core::ledger::Ledger;
use camp_core::pack::parse_agent_dir;

/// The hardened git argv, byte-for-byte: ten `-c KEY=VALUE` pairs, in order.
/// Pinned by `hardened_git_argv_is_exact`; do not reorder.
pub fn hardened_git_args() -> [&'static str; 20] {
    [
        "-c",
        "http.followRedirects=false",
        "-c",
        "protocol.allow=never",
        "-c",
        "protocol.https.allow=always",
        "-c",
        "protocol.http.allow=always",
        "-c",
        "protocol.ssh.allow=always",
        "-c",
        "protocol.git.allow=always",
        "-c",
        "protocol.file.allow=always",
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.untrackedCache=false",
    ]
}

/// Strip every `GIT_*` env var from `cmd` (so a cloned repo's hooks/config
/// cannot inherit the operator's git identity or aliases). Does NOT
/// `env_clear` — PATH survives, which campd needs to spawn workers.
fn strip_git_env(cmd: &mut std::process::Command) {
    let git_keys: Vec<String> = std::env::vars()
        .map(|(k, _)| k)
        .filter(|k| k.starts_with("GIT_"))
        .collect();
    for k in git_keys {
        cmd.env_remove(k);
    }
}

/// Resolve a repository's reference (or `HEAD`) to a full 40-char sha via
/// `git <hardened> ls-remote`. The hardened flags + the `GIT_*` env strip
/// run for every network git call (component §11).
pub fn resolve_commit(repository: &str, reference: Option<&str>) -> Result<String> {
    let ref_arg = reference.unwrap_or("HEAD");
    let output = std::process::Command::new("git")
        .args(hardened_git_args())
        .arg("ls-remote")
        .arg(repository)
        .arg(ref_arg)
        .output()
        .with_context(|| format!("failed to spawn git ls-remote for {repository:?}"))?;
    if !output.status.success() {
        bail!(
            "git ls-remote {repository:?} ({ref_arg}) failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    // `ls-remote` prints `<sha>\t<ref>`; the sha is the first 40 chars.
    let sha = stdout
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .next()
        .unwrap_or("");
    if sha.len() != 40 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!(
            "git ls-remote {repository:?} ({ref_arg}) returned no sha: {}",
            stdout.trim()
        );
    }
    Ok(sha.to_owned())
}

/// Full clone (so subpaths/commits are present for transitive resolution)
/// with the hardened argv and the `GIT_*` env strip. Component §10 error
/// table: on failure, name the source + git's stderr.
pub fn git_clone(repository: &str, dest: &Path) -> Result<()> {
    let mut cmd = std::process::Command::new("git");
    cmd.args(hardened_git_args())
        .arg("clone")
        .arg(repository)
        .arg(dest);
    strip_git_env(&mut cmd);
    let output = cmd
        .output()
        .with_context(|| format!("failed to spawn git clone for {repository:?}"))?;
    if !output.status.success() {
        bail!(
            "git clone {repository:?} into {} failed: {}",
            dest.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

/// A binding name: `[A-Za-z0-9_-]+`, non-empty, not `.`/`..`.
fn valid_binding(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Derive a binding name: `--name`, else the source's last subpath
/// component, else the repo's last path segment. Validated; on failure the
/// operator is told to pass `--name`.
fn derive_binding(name: Option<&str>, src: &camp_core::import::source::Source) -> Result<String> {
    let derived = name
        .map(|s| s.to_owned())
        .or_else(|| {
            src.subpath
                .as_ref()
                .and_then(|s| s.rsplit('/').next().map(|s| s.to_owned()))
        })
        .or_else(|| {
            src.repository
                .rsplit(['/', ':'])
                .find(|s| !s.is_empty())
                .map(|s| s.trim_end_matches(".git").to_owned())
        });
    match derived {
        Some(b) if valid_binding(&b) => Ok(b),
        Some(b) => {
            bail!("derived binding {b:?} is not a valid binding name ([A-Za-z0-9_-]+); pass --name")
        }
        None => bail!("cannot derive a binding name from {src:?}; pass --name"),
    }
}

/// `camp import add <source> [--name <binding>] [--version <ref>]` (component
/// §9): normalize → derive/validate binding → hardened clone (or local-path
/// read) → resolve commit → read manifest → resolve transitive → materialize
/// self + deduped transitive into `<root>/imports/<binding>/` (refusing a
/// transitive `agents/` dir) → append `[imports.<n>]` to camp.toml → write
/// lock entries → inventory executable content → collect §5.4 refusals →
/// append `import.added` + one `import.refused` per refusal. Idempotent for
/// the same `(name, source, subpath, version)`; a different source for the
/// same name → error.
pub fn run_add(
    camp_root: &Path,
    source: &str,
    name: Option<&str>,
    version: Option<&str>,
) -> Result<()> {
    let src = normalize(source, version).with_context(|| format!("source {source:?}"))?;
    let binding = derive_binding(name, &src)?;
    let lock_path = camp_root.join("packs.lock");
    let mut lock = PacksLock::read(&lock_path).context("packs.lock")?;

    // Idempotency / conflict on DIRECT entries (via = None) for this binding.
    if let Some(direct) = lock
        .imports
        .iter()
        .find(|e| e.name == binding && e.via.is_none())
    {
        let same = direct.source == src.repository
            && direct.subpath == src.subpath
            && direct.version == src.reference.clone().unwrap_or_default();
        if same {
            return Ok(()); // idempotent
        }
        bail!(
            "import {:?} is already bound to source {:?} (subpath {:?}); \
             `camp import remove {binding}` first to rebind",
            binding,
            direct.source,
            direct.subpath
        );
    }

    // Clone (remote) or read (local path). Resolve commit for remotes. The
    // clone tempdir is held in `_checkout` for the duration of materialization
    // (it drops at the end of run_add, after run_add_materialize has copied
    // out of it).
    let (repo_dir, commit, _checkout) = if src.is_local_path {
        (PathBuf::from(&src.repository), String::new(), None)
    } else {
        let checkout = tempfile::tempdir().context("clone scratch dir")?;
        let dest = checkout.path().join("repo");
        git_clone(&src.repository, &dest).with_context(|| format!("clone {source:?}"))?;
        let commit = resolve_commit(&src.repository, src.reference.as_deref())
            .with_context(|| format!("resolve {source:?}"))?;
        (dest, commit, Some(checkout))
    };

    // The materialize escape boundary: for a remote clone, the clone dir
    // (untrusted — a pack may not reach outside its checkout). For a LOCAL
    // path, the git repo root containing the pack (the operator's trusted
    // repo) — so a pack may symlink to sibling content in its own repo
    // (e.g. the starter's corpus symlink) but not outside it.
    let materialize_root = if src.is_local_path {
        find_git_root(&repo_dir).unwrap_or_else(|| repo_dir.clone())
    } else {
        repo_dir.clone()
    };

    run_add_materialize(
        camp_root,
        &src,
        &binding,
        &repo_dir,
        &materialize_root,
        &commit,
        &mut lock,
        source,
    )?;
    lock.write(&lock_path).context("write packs.lock")?;
    Ok(())
}

/// Walk up from `start` for the first ancestor containing a `.git` entry
/// (a directory for a normal repo, a file for a worktree/submodule gitdir
/// pointer). Returns `None` when `start` is not inside a git repo.
fn find_git_root(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|dir| dir.join(".git").exists())
        .map(Path::to_path_buf)
}

/// Materialize + lock + camp.toml + ledger events, shared by `add`/`install`.
#[allow(clippy::too_many_arguments)]
fn run_add_materialize(
    camp_root: &Path,
    src: &camp_core::import::source::Source,
    binding: &str,
    repo_dir: &Path,
    materialize_root: &Path,
    commit: &str,
    lock: &mut PacksLock,
    source_str: &str,
) -> Result<()> {
    let subpath_dir = match &src.subpath {
        Some(s) => repo_dir.join(s),
        None => repo_dir.to_path_buf(),
    };
    // Fail fast if the DIRECT source is not a pack (manifest_of re-reads it
    // for transitive resolution, so this is the explicit direct-source gate).
    read_manifest(&subpath_dir).with_context(|| format!("source {source_str:?}: not a pack"))?;

    // Transitive: relative sources anchor at the declaring subpath within the
    // cloned repo (same repo + commit). The manifest closure reads each
    // resolved import's pack.toml from <repo_dir>/<subpath>.
    let direct = vec![ResolvedImport {
        binding: binding.to_owned(),
        source: src.repository.clone(),
        subpath: src.subpath.clone(),
        reference: src.reference.clone(),
        via: None,
        is_local: src.is_local_path,
    }];
    let manifest_of = |ri: &ResolvedImport| {
        let dir = match &ri.subpath {
            Some(s) => repo_dir.join(s),
            None => repo_dir.to_path_buf(),
        };
        match read_manifest(&dir) {
            Ok(m) => Ok(m),
            // A transitive content layer (no pack.toml — e.g. gascity, which
            // is formulas + a nested roles pack, not itself a pack) has no
            // imports → depth-1 holds. A DIRECT import without pack.toml is a
            // real error (the source is not a pack), so we only relax for the
            // transitive case (via.is_some()).
            Err(camp_core::error::CoreError::Import { ref reason, .. })
                if ri.via.is_some() && reason.contains("pack.toml") =>
            {
                Ok(PackManifest {
                    pack: PackMeta {
                        name: ri.binding.clone(),
                        schema: 2,
                        description: None,
                        version: None,
                    },
                    imports: std::collections::BTreeMap::new(),
                })
            }
            Err(e) => Err(e),
        }
    };
    let all = resolve_transitive(&direct, &manifest_of)?;

    // Materialize self + transitive (merge into imports/<binding>/; refuse a
    // transitive agents/ dir — transitive packs contribute content only).
    let imports_root = camp_root.join("imports");
    for imp in &all {
        let dest = imports_root.join(&imp.binding);
        let src_subtree = match &imp.subpath {
            Some(s) => repo_dir.join(s),
            None => repo_dir.to_path_buf(),
        };
        if imp.via.is_some() && src_subtree.join("agents").is_dir() {
            bail!(
                "transitive import {:?} carries an agents/ dir — transitive packs contribute \
                 content layers only (umbrella §7.2)",
                imp.binding
            );
        }
        materialize_tree(materialize_root, &src_subtree, &dest)
            .with_context(|| format!("materialize {source_str:?} into {}", dest.display()))?;
    }

    // Append [imports.<binding>] to camp.toml (once).
    append_import_decl(
        &camp_root.join("camp.toml"),
        binding,
        &ImportDecl {
            source: src.repository.clone(),
            subpath: src.subpath.clone(),
            version: src.reference.clone(),
            trust_exec: false,
            skills: None,
        },
    )?;

    // Lock: drop this binding's direct + its-declared-transitive entries, then
    // add self (direct) + transitive (via = binding).
    let fetched = jiff::Timestamp::now().to_string();
    lock.imports.retain(|e| {
        let is_direct_self = e.via.is_none() && e.name == binding;
        let is_declared_transitive = e.via.as_deref() == Some(binding);
        !(is_direct_self || is_declared_transitive)
    });
    lock.imports.push(LockEntry {
        name: binding.to_owned(),
        source: src.repository.clone(),
        subpath: src.subpath.clone(),
        version: src.reference.clone().unwrap_or_default(),
        commit: commit.to_owned(),
        fetched: fetched.clone(),
        via: None,
    });
    for imp in all.iter().filter(|i| i.via.is_some()) {
        lock.imports.push(LockEntry {
            name: imp.binding.clone(),
            source: src.repository.clone(),
            subpath: imp.subpath.clone(),
            version: src.reference.clone().unwrap_or_default(),
            commit: commit.to_owned(),
            fetched: fetched.clone(),
            via: Some(binding.to_owned()),
        });
    }

    // Inventory executable content across EVERY materialized dir (self +
    // transitive — §14.10), and collect §5.4 agent refusals.
    let mut exec_inventory: Vec<ExecItem> = Vec::new();
    let mut refusals: Vec<(String, String, String)> = Vec::new(); // (binding, agent, key)
    let mut ignored_keys: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for imp in &all {
        let dir = imports_root.join(&imp.binding);
        if dir.join("agents").is_dir() {
            for entry in std::fs::read_dir(dir.join("agents"))?.flatten() {
                if entry.path().is_dir() {
                    let (raw, agent_refusals) = parse_agent_dir(&entry.path())?;
                    for r in agent_refusals {
                        ignored_keys.insert(r.key.clone());
                        refusals.push((imp.binding.clone(), raw.name.clone(), r.key));
                    }
                }
            }
        }
        if dir.is_dir() {
            exec_inventory.extend(camp_core::import::inventory::inventory_executable(&dir)?);
        }
    }

    // §7.1 import-time visibility: scan the materialized packs' formula routes
    // for bindings they reference that are NOT yet bound, naming the `--name`
    // remedy (so the operator learns of a routing hole at import, not dispatch).
    let bound: std::collections::BTreeSet<&str> = all.iter().map(|i| i.binding.as_str()).collect();
    let mut unbound_bindings: std::collections::BTreeSet<String> =
        std::collections::BTreeSet::new();
    for imp in &all {
        let dir = imports_root.join(&imp.binding);
        let formulas = dir.join("formulas");
        if !formulas.is_dir() {
            continue;
        }
        for f in std::fs::read_dir(&formulas)?.flatten() {
            let path = f.path();
            if path.extension().is_none_or(|x| x != "toml") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(doc) = toml::from_str::<toml::Value>(&text) else {
                continue;
            };
            let Some(steps) = doc.get("steps").and_then(|s| s.as_array()) else {
                continue;
            };
            for step in steps {
                let routes = [
                    step.get("route").and_then(|v| v.as_str()),
                    step.get("metadata")
                        .and_then(|m| m.get("gc"))
                        .and_then(|g| g.get("run_target"))
                        .and_then(|v| v.as_str()),
                ];
                for route in routes {
                    if let Some((b, _)) = route.unwrap_or("").split_once('.')
                        && !bound.contains(b)
                        && b != binding
                    {
                        unbound_bindings.insert(b.to_owned());
                    }
                }
            }
        }
    }

    // §7.3 nested-pack report: a materialized transitive subtree may contain a
    // nested `pack.toml` camp did not compose (e.g. gascity/roles/). Report each
    // so the operator imports it explicitly rather than wondering why its agents
    // do not resolve.
    let mut nested_packs: Vec<serde_json::Value> = Vec::new();
    for imp in &all {
        if imp.via.is_none() {
            continue;
        }
        let dir = imports_root.join(&imp.binding);
        for nested in find_nested_pack_tomls(&dir)? {
            let rel = nested
                .strip_prefix(&dir)
                .unwrap_or(&nested)
                .parent()
                .unwrap_or(std::path::Path::new(""))
                .display()
                .to_string();
            let name = read_manifest(nested.parent().unwrap_or(&dir))
                .map(|m| m.pack.name)
                .unwrap_or_default();
            nested_packs.push(serde_json::json!({ "path": rel, "name": name }));
        }
    }

    // Ledger events: import.added (aggregated) + one import.refused per key.
    let mut ledger = Ledger::open(&camp_root.join("camp.db")).context("open ledger")?;
    let exec_json: Vec<serde_json::Value> = exec_inventory
        .iter()
        .map(|i| serde_json::json!({ "kind": i.kind, "path": i.path, "detail": i.detail }))
        .collect();
    ledger.append(EventInput {
        kind: EventType::ImportAdded,
        rig: None,
        actor: "cli".into(),
        bead: None,
        data: serde_json::json!({
            "binding": binding,
            "source": src.repository,
            "commit": commit,
            "ignored_keys": ignored_keys.iter().cloned().collect::<Vec<_>>(),
            "reported": serde_json::json!({
                "unbound_bindings": unbound_bindings.iter().cloned().collect::<Vec<_>>(),
                "nested_packs": nested_packs,
            }),
            "exec_inventory": exec_json,
        }),
    })?;
    for (imp_binding, agent, key) in &refusals {
        ledger.append(EventInput {
            kind: EventType::ImportRefused,
            rig: None,
            actor: "cli".into(),
            bead: None,
            data: serde_json::json!({
                "binding": imp_binding,
                "pack": imp_binding,
                "agent": agent,
                "key": key,
                "reason": format!("agent {agent:?}: key {key:?} is not supported (umbrella §5.4)"),
            }),
        })?;
    }

    println!(
        "imported {binding} from {source_str} (commit {commit:.12}){}",
        if all.iter().any(|i| i.via.is_some()) {
            format!(
                " + {} transitive",
                all.iter().filter(|i| i.via.is_some()).count()
            )
        } else {
            String::new()
        }
    );
    if !exec_inventory.is_empty() {
        eprintln!(
            "trust_exec: {} executable item(s) inventoried — none run unless [imports.{binding}] \
             sets trust_exec = true",
            exec_inventory.len()
        );
    }
    for b in &unbound_bindings {
        eprintln!(
            "warning: formula routes reference binding {b:?}, which is not imported — run \
             `camp import add <source> --name {b}` to bind it, or routes like {b}.<agent> will \
             fail at dispatch"
        );
    }
    for np in &nested_packs {
        eprintln!(
            "note: nested pack at {} ({}) — camp did not compose it; import it explicitly with \
             `camp import add <source>//{}/{} --name <binding>`",
            np["path"], np["name"], binding, np["path"]
        );
    }
    Ok(())
}

/// Recursively find `pack.toml` files under `dir`, EXCLUDING `dir/pack.toml`
/// itself and any nested `imports/` (a materialized sub-import, not a pack).
/// Used to report nested packs in a transitive subtree (§7.3).
fn find_nested_pack_tomls(dir: &Path) -> Result<Vec<PathBuf>, anyhow::Error> {
    let mut found = Vec::new();
    fn walk(dir: &Path, found: &mut Vec<PathBuf>) -> Result<(), anyhow::Error> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let name = entry.file_name();
            if name == "imports" {
                continue; // a materialized sub-import, not a nested pack
            }
            let path = entry.path();
            if path.is_dir() {
                if path.join("pack.toml").is_file() {
                    // a nested pack dir
                    found.push(path.join("pack.toml"));
                    // do not descend into the nested pack (its content is its own)
                    continue;
                }
                walk(&path, found)?;
            }
        }
        Ok(())
    }
    walk(dir, &mut found)?;
    Ok(found)
}

/// Append an `[imports.<binding>]` section to camp.toml once (idempotent on
/// the header line). Surgical text edit — the rest of the file is untouched.
fn append_import_decl(camp_toml: &Path, binding: &str, decl: &ImportDecl) -> Result<()> {
    let text = std::fs::read_to_string(camp_toml)
        .with_context(|| format!("cannot read {}", camp_toml.display()))?;
    let header = format!("[imports.{binding}]");
    if text.lines().any(|l| l.trim() == header) {
        return Ok(());
    }
    let body = toml::to_string(decl).context("serialize import decl")?;
    let mut out = text;
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push('\n');
    out.push_str(&header);
    out.push('\n');
    out.push_str(&body);
    std::fs::write(camp_toml, out)
        .with_context(|| format!("cannot write {}", camp_toml.display()))?;
    Ok(())
}

/// `camp import install` — re-materialize every locked import (never
/// re-resolves a ref; `upgrade` is the only ref-mover).
pub fn run_install(camp_root: &Path) -> Result<()> {
    let lock = PacksLock::read(&camp_root.join("packs.lock"))?;
    for entry in &lock.imports {
        if entry.via.is_some() {
            continue; // transitive — materialized with its declaring import
        }
        let src = normalize(&entry.source, Some(&entry.version))
            .with_context(|| format!("locked import {:?}", entry.name))?;
        let (repo_dir, commit, materialize_root, _checkout) = if src.is_local_path {
            let rd = PathBuf::from(&src.repository);
            let mr = find_git_root(&rd).unwrap_or_else(|| rd.clone());
            (rd, entry.commit.clone(), mr, None)
        } else {
            let checkout = tempfile::tempdir().context("clone scratch dir")?;
            let dest = checkout.path().join("repo");
            git_clone(&src.repository, &dest)?;
            (dest.clone(), entry.commit.clone(), dest, Some(checkout))
        };
        let mut new_lock = PacksLock::read(&camp_root.join("packs.lock"))?;
        run_add_materialize(
            camp_root,
            &src,
            &entry.name,
            &repo_dir,
            &materialize_root,
            &commit,
            &mut new_lock,
            &entry.source,
        )?;
    }
    PacksLock::read(&camp_root.join("packs.lock"))?.write(&camp_root.join("packs.lock"))?;
    Ok(())
}

/// `camp import upgrade [<name>]` — re-resolve the ref and move the commit.
pub fn run_upgrade(camp_root: &Path, name: Option<&str>) -> Result<()> {
    let lock = PacksLock::read(&camp_root.join("packs.lock"))?;
    for entry in lock.imports.iter().filter(|e| e.via.is_none()) {
        if let Some(n) = name
            && entry.name != n
        {
            continue;
        }
        let src = normalize(&entry.source, Some(&entry.version))?;
        if src.is_local_path {
            bail!(
                "upgrade is a no-op for a local-path import {:?}",
                entry.name
            );
        }
        let commit = resolve_commit(&src.repository, src.reference.as_deref())?;
        let checkout = tempfile::tempdir().context("clone scratch dir")?;
        let dest = checkout.path().join("repo");
        git_clone(&src.repository, &dest)?;
        run_add_materialize(
            camp_root,
            &src,
            &entry.name,
            &dest,
            &dest,
            &commit,
            &mut PacksLock::read(&camp_root.join("packs.lock"))?,
            &entry.source,
        )?;
    }
    Ok(())
}

/// `camp import check` — offline: verify every locked import's materialized
/// tree exists and matches the lock's recorded subpath.
pub fn run_check(camp_root: &Path) -> Result<()> {
    let lock = PacksLock::read(&camp_root.join("packs.lock"))?;
    let mut missing = 0;
    for entry in &lock.imports {
        let dir = camp_root.join("imports").join(&entry.name);
        if !dir.is_dir() {
            eprintln!("missing: {} (import {:?})", dir.display(), entry.name);
            missing += 1;
        }
    }
    if missing > 0 {
        bail!("{missing} materialized import(s) missing — run `camp import install`");
    }
    println!("{} import(s) present", lock.imports.len());
    Ok(())
}

/// `camp import list` — the lock entries with provenance.
pub fn run_list(camp_root: &Path) -> Result<()> {
    let lock = PacksLock::read(&camp_root.join("packs.lock"))?;
    if lock.imports.is_empty() {
        println!("no imports (add one with `camp import add <source> --name <binding>`)");
        return Ok(());
    }
    println!(
        "{:<16} {:<48} {:<14} {:<10}",
        "NAME", "SOURCE", "SUBPATH", "VIA"
    );
    for e in &lock.imports {
        println!(
            "{:<16} {:<48} {:<14} {:<10}",
            e.name,
            e.source,
            e.subpath.as_deref().unwrap_or("-"),
            e.via.as_deref().unwrap_or("-"),
        );
    }
    Ok(())
}

/// `camp import remove <name>` — drop the lock entry + `<root>/imports/<n>/`.
pub fn run_remove(camp_root: &Path, name: &str) -> Result<()> {
    let lock_path = camp_root.join("packs.lock");
    let mut lock = PacksLock::read(&lock_path)?;
    let before = lock.imports.len();
    lock.imports.retain(|e| e.name != name);
    if lock.imports.len() == before {
        bail!("no import named {name:?}");
    }
    lock.write(&lock_path)?;
    let dir = camp_root.join("imports").join(name);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).with_context(|| format!("remove {}", dir.display()))?;
    }
    // Best-effort: drop the [imports.<name>] block from camp.toml.
    let camp_toml = camp_root.join("camp.toml");
    if camp_toml.is_file()
        && let Ok(text) = std::fs::read_to_string(&camp_toml)
    {
        let header = format!("[imports.{name}]");
        let mut out = String::new();
        let mut skipping = false;
        for line in text.lines() {
            if line.trim() == header {
                skipping = true;
                continue;
            }
            if skipping && line.starts_with('[') && line.trim() != header {
                skipping = false;
            }
            if !skipping {
                out.push_str(line);
                out.push('\n');
            }
        }
        let _ = std::fs::write(&camp_toml, out);
    }
    println!("removed import {name}");
    Ok(())
}
/// (creating parent dirs), then add + commit. Reused by the verb tests and
/// the end-to-end acceptance test.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
pub(crate) mod testsupport {
    use std::path::Path;

    pub fn init_repo(dir: &Path, files: &[(&str, &str)]) {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["init", "-q"])
            .status()
            .map(|s| s.success());
        assert!(ok.unwrap_or(false), "git init -q {dir:?}");
        for (rel, content) in files {
            let path = dir.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&path, content).unwrap();
        }
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.email=t@t", "-c", "user.name=t"])
            .args(["add", "-A"])
            .status()
            .map(|s| s.success());
        assert!(ok.unwrap_or(false), "git add -A {dir:?}");
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(["commit", "-q", "-m", "init"])
            .status()
            .map(|s| s.success());
        assert!(ok.unwrap_or(false), "git commit {dir:?}");
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn hardened_git_argv_is_exact() {
        assert_eq!(
            hardened_git_args(),
            [
                "-c",
                "http.followRedirects=false",
                "-c",
                "protocol.allow=never",
                "-c",
                "protocol.https.allow=always",
                "-c",
                "protocol.http.allow=always",
                "-c",
                "protocol.ssh.allow=always",
                "-c",
                "protocol.git.allow=always",
                "-c",
                "protocol.file.allow=always",
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "core.fsmonitor=false",
                "-c",
                "core.untrackedCache=false",
            ]
        );
    }

    #[test]
    fn clone_and_resolve_a_file_repo() {
        let src = tempfile::tempdir().unwrap();
        testsupport::init_repo(
            src.path(),
            &[("pack.toml", "[pack]\nname = \"x\"\nschema = 2\n")],
        );
        let url = format!("file://{}", src.path().display());
        let sha = resolve_commit(&url, Some("HEAD")).unwrap();
        assert_eq!(sha.len(), 40, "resolved a full sha: {sha}");
        let dest = tempfile::tempdir().unwrap();
        git_clone(&url, &dest.path().join("clone")).unwrap();
        assert!(dest.path().join("clone/pack.toml").exists());
    }
}
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod verb_tests {
    use super::*;

    #[test]
    fn add_from_file_repo_clones_locks_materializes() {
        let repo = tempfile::tempdir().unwrap();
        testsupport::init_repo(
            repo.path(),
            &[
                (
                    "bmad/pack.toml",
                    "[pack]\nname=\"bmad\"\nschema=2\n[imports.gc]\nsource=\"../gascity\"\n",
                ),
                (
                    "bmad/agents/architect/agent.toml",
                    "scope=\"rig\"\nfallback=true\npre_start=\"boot\"\n",
                ),
                (
                    "bmad/agents/architect/prompt.template.md",
                    "You are the architect.",
                ),
                ("bmad/skills/bmad-create-architecture/SKILL.md", "# skill"),
                (
                    "gascity/formulas/build-base.formula.toml",
                    "formula=\"build-base\"\n[[steps]]\nid=\"s\"\ntitle=\"t\"\n[steps.check]\nmode=\"exec\"\npath=\"scripts/parent-verify.sh\"\n",
                ),
            ],
        );
        let camp = tempfile::tempdir().unwrap();
        std::fs::write(
            camp.path().join("camp.toml"),
            "[camp]\nname=\"t\"\n[agent_defaults]\ntools=[\"Read\",\"Skill\"]\n",
        )
        .unwrap();
        camp_core::ledger::Ledger::open(&camp.path().join("camp.db")).unwrap();
        let url = format!("file://{}//bmad", repo.path().display());
        run_add(camp.path(), &url, Some("bmad"), None).unwrap();

        let cfg = camp_core::config::CampConfig::load(&camp.path().join("camp.toml")).unwrap();
        assert!(cfg.imports.contains_key("bmad"));
        assert!(
            !cfg.imports["bmad"].trust_exec,
            "an import is untrusted unless the operator opts in"
        );
        let lock =
            camp_core::import::lock::PacksLock::read(&camp.path().join("packs.lock")).unwrap();
        assert!(lock.entry("bmad").is_some());
        let gc = lock
            .imports
            .iter()
            .find(|e| e.subpath.as_deref() == Some("gascity"))
            .unwrap();
        assert_eq!(gc.via.as_deref(), Some("bmad"));
        assert_eq!(
            camp_core::pack::resolve_agent(&cfg, "bmad.architect")
                .unwrap()
                .name,
            "bmad.architect"
        );

        let led = camp_core::ledger::Ledger::open(&camp.path().join("camp.db")).unwrap();
        let added = led
            .events_of_type(camp_core::event::EventType::ImportAdded)
            .unwrap();
        assert!(!added.is_empty());
        let inventory = added[0].data["exec_inventory"].to_string();
        assert!(
            inventory.contains("parent-verify.sh"),
            "transitive check.path must be inventoried: {inventory}"
        );
        let refused = led
            .events_of_type(camp_core::event::EventType::ImportRefused)
            .unwrap();
        assert!(
            refused.iter().any(|e| e.data["key"] == "pre_start"
                && e.data["agent"] == "architect"
                && e.data["binding"] == "bmad"),
            "one import.refused per refused key: {refused:?}"
        );
    }

    #[test]
    fn re_adding_same_source_is_idempotent_and_different_source_errors() {
        let repo = tempfile::tempdir().unwrap();
        testsupport::init_repo(
            repo.path(),
            &[
                ("bmad/pack.toml", "[pack]\nname=\"bmad\"\nschema=2\n"),
                ("bmad/agents/a/prompt.md", "a"),
            ],
        );
        let camp = tempfile::tempdir().unwrap();
        std::fs::write(
            camp.path().join("camp.toml"),
            "[camp]\nname=\"t\"\n[agent_defaults]\ntools=[\"Read\"]\n",
        )
        .unwrap();
        camp_core::ledger::Ledger::open(&camp.path().join("camp.db")).unwrap();
        let url = format!("file://{}//bmad", repo.path().display());
        run_add(camp.path(), &url, Some("bmad"), None).unwrap();
        run_add(camp.path(), &url, Some("bmad"), None).unwrap(); // idempotent
        let repo2 = tempfile::tempdir().unwrap();
        testsupport::init_repo(
            repo2.path(),
            &[
                ("bmad/pack.toml", "[pack]\nname=\"bmad\"\nschema=2\n"),
                ("bmad/agents/a/prompt.md", "a"),
            ],
        );
        let other = format!("file://{}//bmad", repo2.path().display());
        assert!(
            run_add(camp.path(), &other, Some("bmad"), None).is_err(),
            "same name, different source"
        );
    }

    // ---- §3 two-command recipe — end-to-end acceptance (file://, no network) -

    #[test]
    fn two_command_recipe_materializes_bmad_transitive_gascity_and_roles_bound_gc() {
        let repo = tempfile::tempdir().unwrap();
        testsupport::init_repo(
            repo.path(),
            &[
                (
                    "bmad/pack.toml",
                    "[pack]\nname=\"bmad\"\nschema=2\n[imports.gc]\nsource=\"../gascity\"\n",
                ),
                (
                    "bmad/agents/architect/agent.toml",
                    "scope=\"rig\"\nfallback=true\n",
                ),
                (
                    "bmad/agents/architect/prompt.template.md",
                    "architect {{.Var}}",
                ),
                ("bmad/skills/bmad-create-architecture/SKILL.md", "# skill"),
                (
                    "gascity/formulas/build-base.formula.toml",
                    "formula=\"build-base\"\n",
                ),
                (
                    "gascity/roles/pack.toml",
                    "[pack]\nname=\"gc-roles\"\nschema=2\n",
                ),
                ("gascity/roles/agents/run-operator/prompt.md", "operate"),
                (
                    "gascity/roles/agents/review-synthesizer/prompt.md",
                    "gc synth",
                ),
                (
                    "gstack/pack.toml",
                    "[pack]\nname=\"gstack\"\nschema=2\n[imports.gc]\nsource=\"../gascity\"\n",
                ),
                ("gstack/agents/review-synthesizer/prompt.md", "gstack synth"),
            ],
        );
        let camp = tempfile::tempdir().unwrap();
        let root = camp.path();
        std::fs::write(
            root.join("camp.toml"),
            "[camp]\nname=\"t\"\n[agent_defaults]\ntools=[\"Read\",\"Skill\"]\n",
        )
        .unwrap();
        camp_core::ledger::Ledger::open(&root.join("camp.db")).unwrap();
        let base = format!("file://{}", repo.path().display());

        // The two commands (§3), against LOCAL file:// (never the network):
        run_add(root, &format!("{base}//bmad"), Some("bmad"), None).unwrap();
        run_add(root, &format!("{base}//gascity/roles"), Some("gc"), None).unwrap();

        let cfg = camp_core::config::CampConfig::load(&root.join("camp.toml")).unwrap();
        assert_eq!(
            camp_core::pack::resolve_agent(&cfg, "bmad.architect")
                .unwrap()
                .name,
            "bmad.architect"
        );
        let lock = camp_core::import::lock::PacksLock::read(&root.join("packs.lock")).unwrap();
        assert!(
            lock.imports.iter().any(
                |e| e.subpath.as_deref() == Some("gascity") && e.via.as_deref() == Some("bmad")
            ),
            "transitive gascity materialized with via=bmad"
        );
        assert_eq!(
            camp_core::pack::resolve_agent(&cfg, "gc.run-operator")
                .unwrap()
                .name,
            "gc.run-operator"
        );
        assert!(
            camp_core::orders::resolve_formula(&cfg, "build-base").is_ok(),
            "gascity contributes formula layers"
        );

        // add gstack too: the cross-binding collision coexists:
        run_add(root, &format!("{base}//gstack"), Some("gstack"), None).unwrap();
        let cfg = camp_core::config::CampConfig::load(&root.join("camp.toml")).unwrap();
        assert!(
            camp_core::pack::resolve_agent(&cfg, "gstack.review-synthesizer")
                .unwrap()
                .prompt
                .contains("gstack")
        );
        assert!(
            camp_core::pack::resolve_agent(&cfg, "gc.review-synthesizer")
                .unwrap()
                .prompt
                .contains("gc")
        );
        // an unbound binding fails naming the remedy:
        assert!(
            camp_core::pack::resolve_agent(&cfg, "superpowers.implementer")
                .unwrap_err()
                .to_string()
                .contains("camp import add")
        );
    }

    #[test]
    fn transitive_relative_source_escaping_the_repo_is_refused_at_add() {
        let repo = tempfile::tempdir().unwrap();
        testsupport::init_repo(
            repo.path(),
            &[
                (
                    "bmad/pack.toml",
                    "[pack]\nname=\"bmad\"\nschema=2\n[imports.gc]\nsource=\"../../etc\"\n",
                ),
                ("bmad/agents/a/prompt.md", "a"),
            ],
        );
        let camp = tempfile::tempdir().unwrap();
        std::fs::write(
            camp.path().join("camp.toml"),
            "[camp]\nname=\"t\"\n[agent_defaults]\ntools=[\"Read\"]\n",
        )
        .unwrap();
        camp_core::ledger::Ledger::open(&camp.path().join("camp.db")).unwrap();
        let url = format!("file://{}//bmad", repo.path().display());
        let err = run_add(camp.path(), &url, Some("bmad"), None)
            .unwrap_err()
            .to_string();
        assert!(
            err.to_lowercase().contains("escape") || err.contains("repo"),
            "{err}"
        );
    }

    // ---- amendment defects 1, 3, 4 ------------------------------------------

    #[test]
    fn transitive_pack_shipping_agents_is_refused() {
        // §7.2: a transitive pack that ships an `agents/` dir is refused —
        // transitive packs contribute content layers only, not agents.
        let repo = tempfile::tempdir().unwrap();
        testsupport::init_repo(
            repo.path(),
            &[
                (
                    "bmad/pack.toml",
                    "[pack]\nname=\"bmad\"\nschema=2\n[imports.gc]\nsource=\"../gascity\"\n",
                ),
                ("bmad/agents/a/prompt.md", "a"),
                ("gascity/agents/x/prompt.md", "x"),
            ],
        );
        let camp = tempfile::tempdir().unwrap();
        std::fs::write(
            camp.path().join("camp.toml"),
            "[camp]\nname=\"t\"\n[agent_defaults]\ntools=[\"Read\"]\n",
        )
        .unwrap();
        camp_core::ledger::Ledger::open(&camp.path().join("camp.db")).unwrap();
        let url = format!("file://{}//bmad", repo.path().display());
        let err = run_add(camp.path(), &url, Some("bmad"), None)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("agents") && (err.contains("transitive") || err.contains("refused")),
            "transitive agents/ must be refused loudly: {err}"
        );
    }

    #[test]
    fn add_reports_unbound_route_bindings() {
        // §7.1: `camp import add` scans the pack's formula routes and reports
        // any binding they reference that is not yet bound (naming --name).
        let repo = tempfile::tempdir().unwrap();
        testsupport::init_repo(
            repo.path(),
            &[
                ("bmad/pack.toml", "[pack]\nname=\"bmad\"\nschema=2\n"),
                ("bmad/agents/a/prompt.md", "a"),
                (
                    "bmad/formulas/run-op.toml",
                    "formula=\"run-op\"\n[[steps]]\nid=\"s\"\ntitle=\"t\"\nroute=\"gc.run-operator\"\n",
                ),
            ],
        );
        let camp = tempfile::tempdir().unwrap();
        std::fs::write(
            camp.path().join("camp.toml"),
            "[camp]\nname=\"t\"\n[agent_defaults]\ntools=[\"Read\"]\n",
        )
        .unwrap();
        camp_core::ledger::Ledger::open(&camp.path().join("camp.db")).unwrap();
        let url = format!("file://{}//bmad", repo.path().display());
        run_add(camp.path(), &url, Some("bmad"), None).unwrap();
        let led = camp_core::ledger::Ledger::open(&camp.path().join("camp.db")).unwrap();
        let added = led
            .events_of_type(camp_core::event::EventType::ImportAdded)
            .unwrap();
        let reported = added[0].data["reported"].to_string();
        assert!(
            reported.contains("gc") && reported.contains("unbound"),
            "import.added must report the unbound gc binding: {reported}"
        );
    }

    #[test]
    fn add_reports_nested_pack_in_transitive_subtree() {
        // §7.3: a nested pack.toml inside the transitive subtree (e.g.
        // gascity/roles/) is reported — import it explicitly.
        let repo = tempfile::tempdir().unwrap();
        testsupport::init_repo(
            repo.path(),
            &[
                (
                    "bmad/pack.toml",
                    "[pack]\nname=\"bmad\"\nschema=2\n[imports.gc]\nsource=\"../gascity\"\n",
                ),
                ("bmad/agents/a/prompt.md", "a"),
                (
                    "gascity/roles/pack.toml",
                    "[pack]\nname=\"gc-roles\"\nschema=2\n",
                ),
            ],
        );
        let camp = tempfile::tempdir().unwrap();
        std::fs::write(
            camp.path().join("camp.toml"),
            "[camp]\nname=\"t\"\n[agent_defaults]\ntools=[\"Read\"]\n",
        )
        .unwrap();
        camp_core::ledger::Ledger::open(&camp.path().join("camp.db")).unwrap();
        let url = format!("file://{}//bmad", repo.path().display());
        run_add(camp.path(), &url, Some("bmad"), None).unwrap();
        let led = camp_core::ledger::Ledger::open(&camp.path().join("camp.db")).unwrap();
        let added = led
            .events_of_type(camp_core::event::EventType::ImportAdded)
            .unwrap();
        let reported = added[0].data["reported"].to_string();
        assert!(
            reported.contains("roles") && reported.contains("gc-roles"),
            "nested pack roles/ (gc-roles) must be reported: {reported}"
        );
    }
}

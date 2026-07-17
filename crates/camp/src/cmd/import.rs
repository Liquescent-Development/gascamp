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
use camp_core::config::{ImportDecl, TRANSITIVE_DIR};
use camp_core::event::{EventInput, EventType};
use camp_core::import::ResolvedImport;
use camp_core::import::inventory::ExecItem;
use camp_core::import::lock::{LockEntry, PacksLock};
use camp_core::import::manifest::read_manifest;
use camp_core::import::manifest::{PackManifest, PackMeta};
use camp_core::import::materialize::materialize_tree;
use camp_core::import::resolve_transitive;
use camp_core::import::source::{Source, normalize};
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

/// Check `commit` out (detached) in an already-cloned `dest`, with the same
/// hardened argv + `GIT_*` env strip.
///
/// This is what makes the pin REAL. `git clone` alone always lands on the
/// remote's default-branch HEAD, so without this the materialized tree was
/// whatever the branch tip said at fetch time while `packs.lock` and the
/// `import.added` event recorded the sha of the ref the operator asked for —
/// a lock that actively LIED about the bytes it pinned. Those bytes become an
/// agent prompt and a formula, so the pin is the supply-chain boundary §13's
/// hardening rests on. A failure here is fatal: materializing content we
/// cannot prove is the pinned content is exactly the outcome being prevented.
pub fn git_checkout(dest: &Path, commit: &str) -> Result<()> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C")
        .arg(dest)
        .args(hardened_git_args())
        .args(["checkout", "--detach"])
        .arg(commit);
    strip_git_env(&mut cmd);
    let output = cmd.output().with_context(|| {
        format!(
            "failed to spawn git checkout {commit} in {}",
            dest.display()
        )
    })?;
    if !output.status.success() {
        bail!(
            "git checkout {commit} in {} failed: {}",
            dest.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

/// Clone `repository` into `dest` and check out exactly `commit` — the only
/// sanctioned way to obtain a remote tree, so no caller can forget the pin.
fn clone_at_commit(repository: &str, dest: &Path, commit: &str) -> Result<()> {
    git_clone(repository, dest)?;
    git_checkout(dest, commit)
}

/// The `Source` a lock entry describes. Reconstructed FIELD-BY-FIELD, never by
/// re-parsing `entry.source`: the `//subpath` was split off into `entry.subpath`
/// at add time, so re-normalizing the bare repository silently dropped it and
/// sent `install`/`upgrade` looking for a pack.toml at the repo ROOT — which
/// broke every subpath import, i.e. all four corpus packs.
fn source_of(entry: &LockEntry) -> Source {
    Source {
        repository: entry.source.clone(),
        subpath: entry.subpath.clone(),
        reference: (!entry.version.is_empty()).then(|| entry.version.clone()),
        is_local_path: camp_core::import::source::is_local_source(&entry.source),
    }
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
    skills: Option<bool>,
) -> Result<()> {
    let src = normalize(source, version).with_context(|| format!("source {source:?}"))?;
    let binding = derive_binding(name, &src)?;
    let lock_path = camp_root.join("packs.lock");
    let mut lock = PacksLock::read(&lock_path).context("packs.lock")?;

    // gc parity: `add` NEVER mutates an existing import — gc errors on any
    // re-add at all (`importsvc.ErrImportExists`). camp keeps the identical
    // re-add idempotent (the container entrypoint re-runs it), but an explicit
    // `--skills` that DIFFERS from the recorded decl is a real operator intent
    // `add` cannot honor: `append_import_decl` is idempotent on the header, so
    // the value would be silently discarded — shipping the very defect #118
    // exists to fix, an opt-out that opts out of nothing. Fail fast instead,
    // naming both remedies.
    //
    // Keyed on the camp.toml DECLARATION, not the lock: a local-path import is
    // layered in place and gets NO lock entry at all (§5/D7), so a lock-based
    // guard silently misses every local import — which is exactly how the first
    // cut of this check passed its file:// test and still did nothing in a real
    // local re-add. camp.toml is also the state `pack.rs` actually reads.
    //
    // Compared on EFFECTIVE values: the sole consumer is
    // `decl.skills != Some(false)` (pack.rs), so an absent key and
    // `skills = true` are the SAME state (install). Comparing raw Options would
    // fail `--skills true` on a default import as though it could not be changed
    // when nothing needs changing — breaking the re-add idempotency the
    // entrypoint depends on.
    if let Some(want) = skills {
        let camp_toml = camp_root.join("camp.toml");
        if camp_toml.exists() {
            let cfg = camp_core::config::CampConfig::load(&camp_toml)?;
            if let Some(decl) = cfg.imports.get(&binding) {
                let current = decl.skills.unwrap_or(true);
                if current != want {
                    bail!(
                        "import {binding:?} already exists — `camp import add --skills {want}` \
                         cannot change it (add never mutates an existing import). Set \
                         `skills = {want}` under [imports.{binding}] in camp.toml, or \
                         `camp import remove {binding}` and add it again with --skills {want}."
                    );
                }
            }
        }
    }

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
            // The explicit-`--skills` conflict is already refused above, keyed on
            // the camp.toml decl (which a local import has and the lock does not).
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

    // Clone (remote) or read (local path). Resolve the commit for remotes and
    // check it out, so the tree we materialize IS the tree we lock. The clone
    // tempdir is held in `_checkout` for the duration of materialization (it
    // drops at the end of run_add, after run_add_materialize has copied out).
    let (repo_dir, anchor_sub, commit, _checkout) = if src.is_local_path {
        let (root, sub) = local_anchor(camp_root, &src.repository)?;
        (root, sub, String::new(), None)
    } else {
        let checkout = tempfile::tempdir().context("clone scratch dir")?;
        let dest = checkout.path().join("repo");
        let commit = resolve_commit(&src.repository, src.reference.as_deref())
            .with_context(|| format!("resolve {source:?}"))?;
        clone_at_commit(&src.repository, &dest, &commit)
            .with_context(|| format!("clone {source:?}"))?;
        (dest, src.subpath.clone(), commit, Some(checkout))
    };

    run_add_materialize(
        camp_root,
        &src,
        &binding,
        &repo_dir,
        &anchor_sub,
        &commit,
        &mut lock,
        source,
        skills,
    )?;
    lock.write(&lock_path).context("write packs.lock")?;
    Ok(())
}

/// Anchor a LOCAL pack for transitive resolution: the repo (or containing
/// directory) it sits in, plus the pack's own subpath relative to that root.
///
/// A pack-level relative source anchors at the DECLARING pack (§7.2), which
/// only resolves if the declaring pack has a subpath to pop `..` off. A local
/// source normalizes to `subpath: None`, so `../gascity` was popping past a
/// lexically EMPTY root and every real pack — all four in the corpus declare
/// exactly that import — was refused. Anchoring at the pack's repo root (its
/// parent, when it is not in a repo) makes `../gascity` resolve to the sibling
/// it is, while the existing escape check still guards the real root.
///
/// The returned root is ALSO the materialize escape boundary: a pack may
/// symlink to sibling content inside its own repo (the starter's corpus
/// symlink) but never outside it.
fn local_anchor(camp_root: &Path, source: &str) -> Result<(PathBuf, Option<String>)> {
    // A relative source in camp.toml is relative to camp.toml (§5); an
    // absolute one is itself. `join` gives both.
    let pack_dir = camp_root.join(source);
    if !pack_dir.is_dir() {
        bail!(
            "local import source {source:?} is not a directory (looked in {})",
            pack_dir.display()
        );
    }
    let pack_dir = pack_dir
        .canonicalize()
        .with_context(|| format!("canonicalize local import source {source:?}"))?;
    let root = find_git_root(&pack_dir)
        .or_else(|| pack_dir.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| pack_dir.clone());
    let sub = pack_dir
        .strip_prefix(&root)
        .ok()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .filter(|s| !s.is_empty());
    Ok((root, sub))
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
    anchor_sub: &Option<String>,
    commit: &str,
    lock: &mut PacksLock,
    source_str: &str,
    skills: Option<bool>,
) -> Result<()> {
    // `anchor_sub` is where the pack sits INSIDE `repo_dir` — the anchor for
    // transitive `..` resolution. For a remote it is the source's `//subpath`;
    // for a local pack it is its path within its own repo (see `local_anchor`),
    // which is what lets `../gascity` resolve to the sibling it is. `repo_dir`
    // is also the materialize escape boundary: a pack may reach sideways inside
    // its own repo, never outside it.
    let subpath_dir = match anchor_sub {
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
        subpath: anchor_sub.clone(),
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

    // The declaration this import will carry in camp.toml. It is also what
    // decides WHERE the import's content is read from (`layer_dir`), so the
    // materialize, inventory, and route-scan passes below all agree with the
    // resolvers by construction — one seam, not four copies of the rule.
    let decl = ImportDecl {
        source: src.repository.clone(),
        subpath: src.subpath.clone(),
        version: src.reference.clone(),
        trust_exec: false,
        skills,
    };
    // Where each resolved import's content lives on disk (D7/D8).
    let layer_of = |imp: &ResolvedImport| -> PathBuf {
        match &imp.via {
            Some(_) => camp_core::config::transitive_layer_dir(camp_root, &imp.binding),
            None => decl.layer_dir(camp_root, &imp.binding),
        }
    };

    // Materialize. Two rules, both from the operator's rulings on #80:
    //
    // D7 — a LOCAL-path direct import is layered IN PLACE: nothing is copied,
    //   so there is no stale duplicate of the operator's own pack to drift.
    //   Its resolvers read `layer_dir` (= the source), so skipping the copy
    //   here IS the implementation.
    // D8 — a TRANSITIVE import materializes under the `.transitive` sentinel,
    //   DISJOINT from `imports/<binding>/`. A direct import of the same binding
    //   therefore OVERRIDES it (§7.1) without merging into or clobbering the
    //   transitive content layer beneath (which the corpus's `extends` needs).
    //   A transitive `agents/` dir stays refused — content layers only (§7.2).
    for imp in &all {
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
        if imp.via.is_none() && src.is_local_path {
            continue; // D7: layered in place — never copied
        }
        let dest = layer_of(imp);
        materialize_tree(repo_dir, &src_subtree, &dest)
            .with_context(|| format!("materialize {source_str:?} into {}", dest.display()))?;
    }

    // Append [imports.<binding>] to camp.toml (once).
    append_import_decl(&camp_root.join("camp.toml"), binding, &decl)?;

    // Lock: drop this binding's direct + its-declared-transitive entries, then
    // add self (direct) + transitive (via = binding).
    //
    // D7: a LOCAL-path import contributes NO lock entries — not for itself and
    // not for anything it pulls in transitively. The lock exists to reproduce a
    // fetch by commit; a local path has no commit to pin, and writing an entry
    // with an empty commit would be a lock that reproduces nothing. `packs.lock`
    // stays a truthful record of exactly what was fetched (§5).
    lock.imports.retain(|e| {
        let is_direct_self = e.via.is_none() && e.name == binding;
        let is_declared_transitive = e.via.as_deref() == Some(binding);
        !(is_direct_self || is_declared_transitive)
    });
    if !src.is_local_path {
        let fetched = jiff::Timestamp::now().to_string();
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
    }

    // Inventory executable content across EVERY import layer (self +
    // transitive — §14.10), and collect §5.4 agent refusals. A local import is
    // inventoried IN PLACE: read-in-place must never mean un-inspected.
    let mut exec_inventory: Vec<ExecItem> = Vec::new();
    let mut refusals: Vec<(String, String, String)> = Vec::new(); // (binding, agent, key)
    let mut ignored_keys: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for imp in &all {
        let dir = layer_of(imp);
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
        let dir = layer_of(imp);
        let formulas = dir.join("formulas");
        if !formulas.is_dir() {
            continue;
        }
        for f in std::fs::read_dir(&formulas)?.flatten() {
            let path = f.path();
            if path.extension().is_none_or(|x| x != "toml") {
                continue;
            }
            // Invariant 5: an unreadable or malformed formula is surfaced, not
            // skipped. Swallowing it silently drops the very routes this scan
            // exists to report, so the operator would be told a pack has no
            // unbound bindings when camp simply could not read it.
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("read formula {}", path.display()))?;
            let doc: toml::Value = toml::from_str(&text)
                .with_context(|| format!("formula {}: invalid TOML", path.display()))?;
            let Some(steps) = doc.get("steps").and_then(|s| s.as_array()) else {
                continue; // a formula with no steps declares no routes
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
    // The subpaths already bound by this import (direct + transitive). A nested
    // pack.toml whose real subpath is one of these is NOT un-composed — it is a
    // pack already imported under a binding, so reporting it would tell the
    // operator to import what they just did (#139: gascity/roles imports its
    // parent `..`, whose subtree re-contains roles/).
    // Canonicalize both sides: a transitive subpath is already clean, but the
    // DIRECT import's subpath is the operator's verbatim `//subpath`, which may
    // carry a trailing slash or `.` segment. Compare on the canonical form so
    // `gascity/roles/` and `gascity/roles` are the same pack.
    let bound_subpaths: std::collections::BTreeSet<String> = all
        .iter()
        .filter_map(|i| i.subpath.as_deref())
        .map(canon_subpath)
        .collect();
    let mut nested_packs: Vec<NestedPack> = Vec::new();
    for imp in &all {
        if imp.via.is_none() {
            continue;
        }
        let dir = layer_of(imp);
        for nested in find_nested_pack_tomls(&dir)? {
            // `rel` is the nested pack's path RELATIVE to the materialized
            // transitive layer (e.g. `roles`) — a camp-internal location.
            let rel = nested
                .strip_prefix(&dir)
                .unwrap_or(&nested)
                .parent()
                .unwrap_or(std::path::Path::new(""))
                .to_string_lossy()
                .replace('\\', "/");
            // Its REAL location in the source repo: the transitive import's own
            // subpath (e.g. `gascity`) joined with `rel` (#137). That — not the
            // top-level binding — is what an explicit re-import must name.
            let source_subpath = match &imp.subpath {
                Some(s) if !rel.is_empty() => format!("{s}/{rel}"),
                Some(s) => s.clone(),
                None => rel.clone(),
            };
            // Already bound under some import — not an un-composed nested pack.
            if bound_subpaths.contains(&canon_subpath(&source_subpath)) {
                continue;
            }
            let name = read_manifest(nested.parent().unwrap_or(&dir))
                .map(|m| m.pack.name)
                .unwrap_or_default();
            // A runnable `camp import add` source for exactly this nested pack.
            // Remote: the same repo + ref, subpath swapped to the real one.
            // Local: the pack's directory on disk (`repo_dir` is the source
            // repo root, never camp's internal `.transitive` copy).
            let import_source = if src.is_local_path {
                repo_dir.join(&source_subpath).display().to_string()
            } else {
                let mut s = format!("{}//{}", src.repository, source_subpath);
                if let Some(reference) = &src.reference {
                    s.push('#');
                    s.push_str(reference);
                }
                s
            };
            nested_packs.push(NestedPack {
                source_subpath,
                name,
                import_source,
            });
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
                "nested_packs": nested_packs.iter().map(|np| serde_json::json!({
                    "subpath": np.source_subpath,
                    "name": np.name,
                    "import_source": np.import_source,
                })).collect::<Vec<_>>(),
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
        eprintln!("{}", nested_pack_note(np));
    }
    Ok(())
}

/// Canonicalize a subpath for identity comparison: drop empty and `.`
/// segments so `gascity/roles/`, `gascity//roles`, and `gascity/roles` are the
/// same pack. Transitive subpaths arrive already normalized; a direct import's
/// subpath is the operator's verbatim `//subpath`, so this is what lets the
/// already-bound check (#139) survive a trailing slash.
fn canon_subpath(s: &str) -> String {
    s.split('/')
        .filter(|p| !p.is_empty() && *p != ".")
        .collect::<Vec<_>>()
        .join("/")
}

/// A nested pack (§7.3) camp materialized but did not compose. `gc` does not
/// auto-compose nested packs either (its `DiscoverPackAgents` scans
/// `<pack>/agents/` only), so the corpus deploys `gascity/roles` by exactly
/// the explicit import this reports.
struct NestedPack {
    /// The nested pack's location in the SOURCE repo — the transitive import's
    /// own subpath joined with the pack's path inside that layer (e.g.
    /// `gascity/roles`). NOT the top-level binding, NOT the layer-relative path.
    source_subpath: String,
    /// The nested pack's declared `[pack].name` (e.g. `gc-roles`).
    name: String,
    /// A copy-pasteable `camp import add` source that re-imports this pack:
    /// remote → `<repo>//<source_subpath>[#<ref>]`; local → the pack's dir.
    import_source: String,
}

/// Render the §7.3 nested-pack note (#137): name the un-composed nested pack,
/// its real source location, and a runnable `camp import add` that imports it
/// explicitly. Only the binding is left as a placeholder — it is the one value
/// the operator must choose (it must match the routes that reference it).
fn nested_pack_note(np: &NestedPack) -> String {
    format!(
        "note: nested pack {:?} at {} was not composed — gc does not auto-compose nested packs \
         either, so import it explicitly to use its agents:\n  \
         camp import add \"{}\" --name <binding>",
        np.name, np.source_subpath, np.import_source
    )
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

/// `camp import install` — re-materialize every locked import AT ITS LOCKED
/// COMMIT (never re-resolves a ref; `upgrade` is the only ref-mover). This is
/// what the docker entrypoint runs on every container start, so it must
/// reproduce the locked tree byte-for-byte.
pub fn run_install(camp_root: &Path) -> Result<()> {
    let lock_path = camp_root.join("packs.lock");
    let mut lock = PacksLock::read(&lock_path)?;
    // The direct entries decide the work; `run_add_materialize` re-derives each
    // one's transitive layers, so iterate a snapshot while `lock` is rewritten.
    let direct: Vec<LockEntry> = lock
        .imports
        .iter()
        .filter(|e| e.via.is_none())
        .cloned()
        .collect();
    for entry in &direct {
        let src = source_of(entry);
        let (repo_dir, anchor_sub, _checkout) = if src.is_local_path {
            let (root, sub) = local_anchor(camp_root, &src.repository)?;
            (root, sub, None)
        } else {
            let checkout = tempfile::tempdir().context("clone scratch dir")?;
            let dest = checkout.path().join("repo");
            // The LOCKED commit — install reproduces, it never re-resolves.
            clone_at_commit(&src.repository, &dest, &entry.commit)
                .with_context(|| format!("locked import {:?}", entry.name))?;
            (dest, src.subpath.clone(), Some(checkout))
        };
        run_add_materialize(
            camp_root,
            &src,
            &entry.name,
            &repo_dir,
            &anchor_sub,
            &entry.commit,
            &mut lock,
            &entry.source,
            // install never rewrites camp.toml (append_import_decl is idempotent
            // on the header), so the operator's existing `skills` value stands.
            None,
        )?;
    }
    lock.write(&lock_path).context("write packs.lock")?;
    Ok(())
}

/// `camp import upgrade [<name>]` — re-resolve the ref, move the commit, and
/// PERSIST it. It is the only ref-mover, so a lock it fails to write leaves the
/// tree ahead of its own pin and every later `install` reproducing the past.
pub fn run_upgrade(camp_root: &Path, name: Option<&str>) -> Result<()> {
    let lock_path = camp_root.join("packs.lock");
    let mut lock = PacksLock::read(&lock_path)?;
    let direct: Vec<LockEntry> = lock
        .imports
        .iter()
        .filter(|e| e.via.is_none())
        .filter(|e| name.is_none_or(|n| e.name == n))
        .cloned()
        .collect();
    if let Some(n) = name
        && direct.is_empty()
    {
        bail!("no import named {n:?} to upgrade");
    }
    for entry in &direct {
        let src = source_of(entry);
        if src.is_local_path {
            bail!(
                "upgrade is a no-op for a local-path import {:?} — it is layered in place, \
                 so editing the source IS the upgrade",
                entry.name
            );
        }
        let commit = resolve_commit(&src.repository, src.reference.as_deref())?;
        let checkout = tempfile::tempdir().context("clone scratch dir")?;
        let dest = checkout.path().join("repo");
        clone_at_commit(&src.repository, &dest, &commit)
            .with_context(|| format!("upgrade {:?}", entry.name))?;
        run_add_materialize(
            camp_root,
            &src,
            &entry.name,
            &dest,
            &src.subpath.clone(),
            &commit,
            &mut lock,
            &entry.source,
            // upgrade moves the commit, not the operator's `skills` opt-out;
            // append_import_decl is idempotent, so the existing value stands.
            None,
        )?;
    }
    lock.write(&lock_path).context("write packs.lock")?;
    Ok(())
}

/// `camp import check` — offline: verify every locked import's materialized
/// tree exists and matches the lock's recorded subpath.
pub fn run_check(camp_root: &Path) -> Result<()> {
    let lock = PacksLock::read(&camp_root.join("packs.lock"))?;
    let mut missing = 0;
    for entry in &lock.imports {
        // Only FETCHED imports are locked (D7: a local path has no entry), and
        // a transitive one materializes under the sentinel dir (D8) — check
        // each where it actually lives, or `check` invents missing imports.
        let dir = match &entry.via {
            Some(_) => camp_core::config::transitive_layer_dir(camp_root, &entry.name),
            None => camp_root.join("imports").join(&entry.name),
        };
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

/// `camp import remove <name>` — drop the binding: its lock entries (its own
/// and the transitive ones it pulled in), its camp-OWNED materialized dirs,
/// and its `[imports.<name>]` block.
///
/// A LOCAL-path import has no lock entry (D7), so presence is decided by
/// `camp.toml` OR the lock — keying off the lock alone would make a local
/// import unremovable. Only camp-owned dirs under `imports/` are deleted:
/// a local import's source is the OPERATOR'S OWN directory, and `remove`
/// unbinds it, it does not delete their pack.
pub fn run_remove(camp_root: &Path, name: &str) -> Result<()> {
    let camp_toml = camp_root.join("camp.toml");
    // A parse/IO failure here is NOT "not declared" — swallowing it would let
    // `remove` delete a tree on the strength of a config it could not read.
    let declared = if camp_toml.is_file() {
        camp_core::config::CampConfig::load(&camp_toml)
            .with_context(|| format!("read {}", camp_toml.display()))?
            .imports
            .contains_key(name)
    } else {
        false
    };

    let lock_path = camp_root.join("packs.lock");
    let mut lock = PacksLock::read(&lock_path)?;
    let before = lock.imports.len();
    lock.imports
        .retain(|e| e.name != name && e.via.as_deref() != Some(name));
    let locked = lock.imports.len() != before;
    if !locked && !declared {
        bail!("no import named {name:?}");
    }

    // camp.toml FIRST: it is the source of truth. If the rewrite fails we have
    // deleted nothing, so the camp stays coherent — the reverse order could
    // delete the tree and then leave camp.toml declaring a gone import.
    if camp_toml.is_file() {
        let text = std::fs::read_to_string(&camp_toml)
            .with_context(|| format!("read {}", camp_toml.display()))?;
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
        std::fs::write(&camp_toml, out)
            .with_context(|| format!("rewrite {}", camp_toml.display()))?;
    }
    lock.write(&lock_path).context("write packs.lock")?;

    // The binding's own materialized dir — camp-owned only. A local import's
    // source is the OPERATOR'S directory: `remove` unbinds it, never deletes it.
    let own = camp_root.join("imports").join(name);
    if own.exists() {
        std::fs::remove_dir_all(&own).with_context(|| format!("remove {}", own.display()))?;
    }
    gc_transitive_layers(camp_root)?;
    println!("removed import {name}");
    Ok(())
}

/// Reclaim every transitive layer NO surviving import still declares.
///
/// Transitive layers are shared and deduped by content: bmad and gstack both
/// declare `[imports.gc] source = "../gascity"`, so they resolve to ONE
/// `.transitive/gc/` layer. Deleting it because the import that happened to
/// materialize it was removed pulls the layer out from under its co-dependent —
/// `check` then fails and every formula that `extends` gascity stops resolving.
/// So ownership is not "who fetched it" but "who still needs it": recompute the
/// needed set from the surviving imports' own manifests and drop only the rest.
fn gc_transitive_layers(camp_root: &Path) -> Result<()> {
    let dir = camp_root.join("imports").join(TRANSITIVE_DIR);
    if !dir.is_dir() {
        return Ok(());
    }
    let camp_toml = camp_root.join("camp.toml");
    let cfg = camp_core::config::CampConfig::load(&camp_toml)
        .with_context(|| format!("read {}", camp_toml.display()))?;

    // What the survivors declare, read from each one's own pack.toml (which
    // lives at its layer dir — in place for a local import, materialized for a
    // remote one). A direct import always has a manifest; a missing one means
    // the camp is inconsistent and we must not guess.
    let mut needed: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for (binding, layer) in cfg.import_layers() {
        let manifest = read_manifest(&layer).with_context(|| {
            format!("import {binding:?}: read pack.toml at {}", layer.display())
        })?;
        needed.extend(manifest.imports.into_keys());
    }

    for entry in std::fs::read_dir(&dir).with_context(|| format!("read {}", dir.display()))? {
        let path = entry
            .with_context(|| format!("read {}", dir.display()))?
            .path();
        if !path.is_dir() {
            continue;
        }
        let Some(binding) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !needed.contains(binding) {
            std::fs::remove_dir_all(&path)
                .with_context(|| format!("reclaim transitive layer {}", path.display()))?;
        }
    }
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

    /// Run a git subcommand in `dir`, asserting success. Hermetic against the
    /// operator's gitconfig (identity + no signing), like `init_repo`.
    pub fn git(dir: &Path, args: &[&str]) {
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
            .args(args)
            .status()
            .map(|s| s.success());
        assert!(ok.unwrap_or(false), "git {args:?} in {dir:?}");
    }

    /// Write `files` into `dir` and commit them as a new revision.
    pub fn commit_files(dir: &Path, msg: &str, files: &[(&str, &str)]) {
        for (rel, content) in files {
            let path = dir.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&path, content).unwrap();
        }
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", msg]);
    }

    /// The sha `rev` resolves to in `dir`.
    pub fn rev_parse(dir: &Path, rev: &str) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["rev-parse", rev])
            .output()
            .unwrap();
        assert!(out.status.success(), "git rev-parse {rev} in {dir:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
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

    /// A camp root with a camp.toml + ledger, ready for `run_add`.
    fn camp_at(dir: &Path) -> &Path {
        std::fs::write(
            dir.join("camp.toml"),
            "[camp]\nname=\"t\"\n[agent_defaults]\ntools=[\"Read\"]\n",
        )
        .unwrap();
        camp_core::ledger::Ledger::open(&dir.join("camp.db")).unwrap();
        dir
    }

    /// A repo whose pack content DIFFERS between tag `v1` and branch HEAD.
    /// Returns (repo, v1_sha, head_sha).
    fn pinned_repo(repo: &Path) -> (String, String) {
        testsupport::init_repo(
            repo,
            &[
                ("bmad/pack.toml", "[pack]\nname=\"bmad\"\nschema=2\n"),
                ("bmad/agents/architect/prompt.md", "V1-PINNED-CONTENT"),
            ],
        );
        testsupport::git(repo, &["tag", "v1"]);
        let v1 = testsupport::rev_parse(repo, "v1");
        testsupport::commit_files(
            repo,
            "move head",
            &[("bmad/agents/architect/prompt.md", "V2-HEAD-CONTENT")],
        );
        let head = testsupport::rev_parse(repo, "HEAD");
        assert_ne!(v1, head, "the fixture must actually move HEAD off the tag");
        (v1, head)
    }

    /// FINDING 1 — the `--version`/`#ref` pin must be REAL. `git clone` alone
    /// always lands on the remote's default-branch HEAD; nothing checked out
    /// the sha `resolve_commit` returned, so the materialized tree was HEAD's
    /// while packs.lock recorded the pinned sha. The lock LIED, and the lie is
    /// load-bearing: it is the supply-chain pin an operator reviews a pack at,
    /// and the bytes it mis-describes are fed to a worker as an agent prompt.
    #[test]
    fn a_pinned_version_materializes_the_pinned_commit_not_head() {
        let repo = tempfile::tempdir().unwrap();
        let (v1, head) = pinned_repo(repo.path());
        let camp = tempfile::tempdir().unwrap();
        let root = camp_at(camp.path());
        let base = format!("file://{}", repo.path().display());

        run_add(
            root,
            &format!("{base}//bmad"),
            Some("bmad"),
            Some("v1"),
            None,
        )
        .unwrap();

        let prompt =
            std::fs::read_to_string(root.join("imports/bmad/agents/architect/prompt.md")).unwrap();
        assert_eq!(
            prompt, "V1-PINNED-CONTENT",
            "the MATERIALIZED bytes must be the pinned commit's, not the branch tip's"
        );
        let lock = PacksLock::read(&root.join("packs.lock")).unwrap();
        let entry = lock.imports.iter().find(|e| e.name == "bmad").unwrap();
        assert_eq!(entry.commit, v1, "the lock records the pinned sha");
        assert_ne!(entry.commit, head, "...which is NOT the branch tip");
    }

    /// FINDING 2 — `install` must re-materialize a SUBPATH import. It rebuilt
    /// the Source with `normalize(entry.source, ..)`, but `entry.source` is the
    /// bare repository (the `//subpath` was split off at add time), so the
    /// subpath was silently dropped and the manifest was read at the repo ROOT.
    /// Every corpus pack lives in a subpath, and the docker entrypoint runs
    /// `install` on every container start — so this failed a restart outright.
    #[test]
    fn install_re_materializes_a_subpath_import() {
        let repo = tempfile::tempdir().unwrap();
        pinned_repo(repo.path());
        let camp = tempfile::tempdir().unwrap();
        let root = camp_at(camp.path());
        let base = format!("file://{}", repo.path().display());
        run_add(root, &format!("{base}//bmad"), Some("bmad"), None, None).unwrap();

        // Simulate the container's empty (gitignored) imports/ dir.
        std::fs::remove_dir_all(root.join("imports")).unwrap();
        run_install(root).expect("install must re-materialize a subpath import");
        assert!(
            root.join("imports/bmad/agents/architect/prompt.md")
                .is_file(),
            "install must restore the pack from its LOCKED subpath"
        );
        run_check(root).expect("check must pass after install");
    }

    /// FINDING 2+3 — `upgrade` is defined as "the only ref-mover", so it must
    /// both re-materialize the new content AND persist the moved commit. It
    /// passed a TEMPORARY `PacksLock` into `run_add_materialize` and never
    /// wrote it, so the lock kept the OLD pin while the tree moved — leaving
    /// the next `install` reasoning from a stale lock.
    #[test]
    fn upgrade_moves_the_locked_commit_and_the_content() {
        let repo = tempfile::tempdir().unwrap();
        testsupport::init_repo(
            repo.path(),
            &[
                ("bmad/pack.toml", "[pack]\nname=\"bmad\"\nschema=2\n"),
                ("bmad/agents/architect/prompt.md", "OLD"),
            ],
        );
        let camp = tempfile::tempdir().unwrap();
        let root = camp_at(camp.path());
        let base = format!("file://{}", repo.path().display());
        run_add(root, &format!("{base}//bmad"), Some("bmad"), None, None).unwrap();
        let before = PacksLock::read(&root.join("packs.lock")).unwrap();
        let old_commit = before
            .imports
            .iter()
            .find(|e| e.name == "bmad")
            .unwrap()
            .commit
            .clone();

        testsupport::commit_files(
            repo.path(),
            "move",
            &[("bmad/agents/architect/prompt.md", "NEW")],
        );
        let new_sha = testsupport::rev_parse(repo.path(), "HEAD");
        assert_ne!(old_commit, new_sha);

        run_upgrade(root, Some("bmad")).unwrap();

        let after = PacksLock::read(&root.join("packs.lock")).unwrap();
        let entry = after.imports.iter().find(|e| e.name == "bmad").unwrap();
        assert_eq!(
            entry.commit, new_sha,
            "upgrade must PERSIST the moved commit — it is the only ref-mover"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("imports/bmad/agents/architect/prompt.md")).unwrap(),
            "NEW",
            "upgrade must re-materialize the new content"
        );
    }

    /// FINDING 4 — `remove` must not delete a transitive layer that a SURVIVING
    /// import still depends on. bmad and gstack both declare `[imports.gc]
    /// source = "../gascity"`, so they SHARE one materialized `.transitive/gc`
    /// layer. Removing bmad deleted it out from under gstack: `check` then
    /// failed, and gstack's formulas that `extends` gascity stopped resolving.
    #[test]
    fn removing_one_import_keeps_a_transitive_layer_its_sibling_still_needs() {
        let repo = tempfile::tempdir().unwrap();
        testsupport::init_repo(
            repo.path(),
            &[
                (
                    "bmad/pack.toml",
                    "[pack]\nname=\"bmad\"\nschema=2\n[imports.gc]\nsource=\"../gascity\"\n",
                ),
                ("bmad/agents/architect/prompt.md", "architect"),
                (
                    "gstack/pack.toml",
                    "[pack]\nname=\"gstack\"\nschema=2\n[imports.gc]\nsource=\"../gascity\"\n",
                ),
                ("gstack/agents/synth/prompt.md", "synth"),
                (
                    "gascity/formulas/build-base.formula.toml",
                    "formula=\"build-base\"\n",
                ),
            ],
        );
        let camp = tempfile::tempdir().unwrap();
        let root = camp_at(camp.path());
        let base = format!("file://{}", repo.path().display());
        run_add(root, &format!("{base}//bmad"), Some("bmad"), None, None).unwrap();
        run_add(root, &format!("{base}//gstack"), Some("gstack"), None, None).unwrap();

        let shared = camp_core::config::transitive_layer_dir(root, "gc");
        assert!(shared.is_dir(), "both imports share one gc layer");

        run_remove(root, "bmad").unwrap();

        assert!(
            shared.is_dir(),
            "the shared transitive layer must SURVIVE — gstack still declares it"
        );
        run_check(root).expect("check must pass: nothing gstack depends on is missing");
        let cfg = camp_core::config::CampConfig::load(&root.join("camp.toml")).unwrap();
        assert!(
            camp_core::orders::resolve_formula(&cfg, "build-base").is_ok(),
            "gstack's transitive formula layer must still resolve"
        );

        // ...and removing the LAST dependent finally reclaims it.
        run_remove(root, "gstack").unwrap();
        assert!(
            !shared.exists(),
            "with no importer left, the transitive layer is reclaimed"
        );
    }

    /// FINDING 6 — a LOCAL-path import of a real pack must work. Every corpus
    /// pack declares `[imports.gc] source = "../gascity"`, and transitive
    /// resolution anchored the declaring pack at a lexical EMPTY root, so
    /// `../gascity` "escaped" and every such import was refused — gutting the
    /// operator's D7 read-in-place ruling in the one scenario it exists for
    /// (`camp init --import <local path>`, docker `CAMP_PACK=/packs/bmad`).
    #[test]
    fn a_local_import_of_a_pack_with_transitive_imports_resolves_the_sibling_layer() {
        let packs = tempfile::tempdir().unwrap();
        // Laid out like the corpus: siblings under one root.
        for (rel, content) in [
            (
                "bmad/pack.toml",
                "[pack]\nname=\"bmad\"\nschema=2\n[imports.gc]\nsource=\"../gascity\"\n",
            ),
            ("bmad/agents/architect/prompt.md", "architect"),
            (
                "gascity/formulas/build-base.formula.toml",
                "formula=\"build-base\"\n",
            ),
        ] {
            let p = packs.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, content).unwrap();
        }
        let camp = tempfile::tempdir().unwrap();
        let root = camp_at(camp.path());

        let bmad = packs.path().join("bmad");
        run_add(root, &bmad.display().to_string(), Some("bmad"), None, None)
            .expect("a local import of a pack declaring [imports.*] must resolve");

        let cfg = camp_core::config::CampConfig::load(&root.join("camp.toml")).unwrap();
        // D7: the pack itself is read IN PLACE (never copied)...
        assert!(!root.join("imports/bmad").exists());
        assert_eq!(
            camp_core::pack::resolve_agent(&cfg, "bmad.architect")
                .unwrap()
                .name,
            "bmad.architect"
        );
        // ...and its transitive sibling layer resolves by bare name.
        assert!(
            camp_core::orders::resolve_formula(&cfg, "build-base").is_ok(),
            "the transitive ../gascity layer must resolve for a LOCAL import too"
        );
    }

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
        run_add(camp.path(), &url, Some("bmad"), None, None).unwrap();

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

    /// issue #118: `camp import add --skills false` must PERSIST `skills = false`
    /// into the written camp.toml's [imports.<binding>] — the opt-out the
    /// dispatcher's skills-install honors. The default (`--skills` omitted =
    /// None) writes no `skills` key.
    #[test]
    fn add_persists_the_skills_opt_out_into_camp_toml() {
        let repo = tempfile::tempdir().unwrap();
        testsupport::init_repo(
            repo.path(),
            &[
                ("bmad/pack.toml", "[pack]\nname=\"bmad\"\nschema=2\n"),
                ("bmad/agents/architect/prompt.md", "architect"),
                ("bmad/skills/bmad-arch/SKILL.md", "# skill"),
            ],
        );
        let camp = tempfile::tempdir().unwrap();
        let root = camp_at(camp.path());
        let url = format!("file://{}//bmad", repo.path().display());

        run_add(root, &url, Some("bmad"), None, Some(false)).unwrap();

        let cfg = camp_core::config::CampConfig::load(&root.join("camp.toml")).unwrap();
        assert_eq!(
            cfg.imports["bmad"].skills,
            Some(false),
            "--skills false must persist skills = false into camp.toml"
        );
    }

    /// #118 review finding 1: a re-add carrying an explicit `--skills` that
    /// DIFFERS from the recorded decl must FAIL, not exit 0 having applied
    /// nothing (the natural path: add the pack, notice skills landing, re-run
    /// with --skills false). gc parity: `add` never mutates an existing import
    /// (gc errors on any re-add: importsvc.ErrImportExists).
    ///
    /// Review round 2: the comparison is on EFFECTIVE values. An absent
    /// `skills` key and `skills = true` are the same state (pack.rs's sole
    /// consumer reads `!= Some(false)`), so `--skills true` on a default import
    /// asks for what already holds and MUST stay an idempotent no-op — a
    /// provisioning script that passes it must not fail on its second run.
    #[test]
    fn re_adding_with_a_differing_explicit_skills_fails_instead_of_silently_ignoring_it() {
        let repo = tempfile::tempdir().unwrap();
        testsupport::init_repo(
            repo.path(),
            &[
                ("bmad/pack.toml", "[pack]\nname=\"bmad\"\nschema=2\n"),
                ("bmad/agents/architect/prompt.md", "architect"),
                ("bmad/skills/bmad-arch/SKILL.md", "# skill"),
            ],
        );
        let camp = tempfile::tempdir().unwrap();
        let root = camp_at(camp.path());
        let url = format!("file://{}//bmad", repo.path().display());

        // first add: no explicit opt-out recorded (skills key absent)
        run_add(root, &url, Some("bmad"), None, None).unwrap();
        let before = std::fs::read_to_string(root.join("camp.toml")).unwrap();

        // --skills false DIFFERS from the effective state (install) → refuse,
        // naming the remedy, and change nothing.
        let err = run_add(root, &url, Some("bmad"), None, Some(false))
            .unwrap_err()
            .to_string();
        assert!(err.contains("already exists"), "must refuse: {err}");
        assert!(err.contains("skills"), "must name the knob: {err}");
        assert!(
            err.contains("camp.toml") || err.contains("import remove"),
            "must name a remedy: {err}"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("camp.toml")).unwrap(),
            before,
            "a refused re-add must leave camp.toml byte-identical"
        );

        // --skills true MATCHES the effective state (absent == install), so it
        // is a no-op, NOT a failure. MUTATION: comparing the raw Option
        // (`current != Some(want)`) fails here — None != Some(true).
        run_add(root, &url, Some("bmad"), None, Some(true))
            .expect("--skills true on a default import asks for what already holds");
        // and a bare re-add stays idempotent
        run_add(root, &url, Some("bmad"), None, None).unwrap();
    }

    /// The same contract for a LOCAL-PATH import — the case a lock-keyed guard
    /// silently misses. A local import is layered in place and gets NO
    /// `packs.lock` entry at all (§5/D7), so `run_add`'s lock-based idempotency
    /// branch never fires for it: the re-add falls through to the fresh-add
    /// path, `append_import_decl` early-returns on the existing header, and an
    /// explicit `--skills` is silently discarded — exit 0, nothing applied,
    /// which is exactly the #118 defect. Caught by driving the real binary, not
    /// by the file:// test above. MUTATION: keying the guard on the lock entry
    /// instead of the camp.toml decl makes this test fail.
    #[test]
    fn a_local_path_import_also_refuses_a_differing_explicit_skills() {
        let camp = tempfile::tempdir().unwrap();
        let root = camp_at(camp.path());
        // a local pack DIRECTORY (not file://) — layered in place, no lock entry
        let pack = camp.path().join("bmadpack");
        std::fs::create_dir_all(pack.join("skills/bmad-arch")).unwrap();
        std::fs::create_dir_all(pack.join("agents/architect")).unwrap();
        std::fs::write(pack.join("pack.toml"), "[pack]\nname=\"bmad\"\nschema=2\n").unwrap();
        std::fs::write(pack.join("agents/architect/prompt.md"), "architect").unwrap();
        std::fs::write(pack.join("skills/bmad-arch/SKILL.md"), "# skill").unwrap();
        let src = pack.display().to_string();

        run_add(root, &src, Some("bmad"), None, None).unwrap();
        // a local import really does get no lock entry — the reason the guard
        // cannot live on the lock
        let lock = PacksLock::read(&root.join("packs.lock")).unwrap();
        assert!(
            !lock.imports.iter().any(|e| e.name == "bmad"),
            "a local import has no lock entry (§5/D7): {:?}",
            lock.imports
        );
        let before = std::fs::read_to_string(root.join("camp.toml")).unwrap();

        let err = run_add(root, &src, Some("bmad"), None, Some(false))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("already exists") && err.contains("skills"),
            "a local re-add must refuse a differing --skills, not exit 0: {err}"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("camp.toml")).unwrap(),
            before,
            "a refused re-add must leave camp.toml byte-identical"
        );
        // matching value stays idempotent
        run_add(root, &src, Some("bmad"), None, Some(true)).unwrap();
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
        run_add(camp.path(), &url, Some("bmad"), None, None).unwrap();
        run_add(camp.path(), &url, Some("bmad"), None, None).unwrap(); // idempotent
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
            run_add(camp.path(), &other, Some("bmad"), None, None).is_err(),
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
        run_add(root, &format!("{base}//bmad"), Some("bmad"), None, None).unwrap();
        run_add(
            root,
            &format!("{base}//gascity/roles"),
            Some("gc"),
            None,
            None,
        )
        .unwrap();

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
        run_add(root, &format!("{base}//gstack"), Some("gstack"), None, None).unwrap();
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

    /// D7 (operator ruling, issue #80) — a LOCAL-path import is layered IN
    /// PLACE: `add` performs NO fetch, writes NO lock entry, and copies
    /// NOTHING under `imports/` (component §5's layout diagram). Every
    /// resolver reads the operator's own directory, resolved relative to
    /// camp.toml — so editing the pack in place is immediately live, with no
    /// re-import step. Mutating `layer_dir` to materialize a local source (or
    /// `run_add` to lock it) turns this red.
    #[test]
    fn a_local_path_import_is_layered_in_place_with_no_copy_and_no_lock_entry() {
        let packs = tempfile::tempdir().unwrap();
        let house = packs.path().join("house");
        std::fs::create_dir_all(house.join("agents/mason")).unwrap();
        std::fs::create_dir_all(house.join("formulas")).unwrap();
        std::fs::write(
            house.join("pack.toml"),
            "[pack]\nname=\"house\"\nschema=2\n",
        )
        .unwrap();
        std::fs::write(house.join("agents/mason/prompt.md"), "lay bricks").unwrap();
        std::fs::write(house.join("formulas/brick.toml"), "formula=\"brick\"\n").unwrap();

        let camp = tempfile::tempdir().unwrap();
        let root = camp.path();
        std::fs::write(
            root.join("camp.toml"),
            "[camp]\nname=\"t\"\n[agent_defaults]\ntools=[\"Read\"]\n",
        )
        .unwrap();
        camp_core::ledger::Ledger::open(&root.join("camp.db")).unwrap();

        // An ABSOLUTE local path (no scheme) — a local source, not a fetch.
        run_add(
            root,
            &house.display().to_string(),
            Some("house"),
            None,
            None,
        )
        .unwrap();

        // No fetch, no copy: `imports/house/` must not exist at all.
        assert!(
            !root.join("imports").join("house").exists(),
            "a local import must NOT be copied under imports/ — it is layered in place"
        );
        // No lock entry: a local path has nothing to pin (§5).
        let lock_path = root.join("packs.lock");
        if lock_path.exists() {
            let lock = camp_core::import::lock::PacksLock::read(&lock_path).unwrap();
            assert!(
                !lock.imports.iter().any(|e| e.name == "house"),
                "a local path has NO lock entry — there is no commit to reproduce"
            );
        }

        // ...yet every resolver reads it, in place.
        let cfg = camp_core::config::CampConfig::load(&root.join("camp.toml")).unwrap();
        let agent = camp_core::pack::resolve_agent(&cfg, "house.mason").unwrap();
        assert_eq!(agent.name, "house.mason");
        assert!(agent.prompt.contains("lay bricks"));
        assert!(
            camp_core::orders::resolve_formula(&cfg, "brick").is_ok(),
            "a local import's formulas join the layers, read in place"
        );

        // Read-in-place is LIVE: editing the source is visible with no re-import.
        std::fs::write(house.join("agents/mason/prompt.md"), "lay MARBLE").unwrap();
        let agent = camp_core::pack::resolve_agent(&cfg, "house.mason").unwrap();
        assert!(
            agent.prompt.contains("lay MARBLE"),
            "in-place layering reads the source, not a stale copy"
        );
    }

    /// D8 (operator ruling, issue #80) — a DIRECT import OVERRIDES a
    /// transitive one for the SAME binding (§7.1; gc's own rule,
    /// pack.go:335-340). This is the §3 recipe's real clash: bmad imports
    /// `../gascity` transitively as `gc`, and the operator then imports
    /// `gascity/roles` DIRECTLY as `gc`.
    ///
    /// The direct import owns `imports/gc/` (its AGENTS resolve), while the
    /// transitive content lands on a SEPARATE path — so the transitive FORMULA
    /// layer survives the override rather than being clobbered or merged into
    /// it. Merging the two into one dir (the old behavior) turns this red.
    #[test]
    fn a_direct_import_overrides_a_transitive_binding_and_the_transitive_formulas_survive() {
        let repo = tempfile::tempdir().unwrap();
        testsupport::init_repo(
            repo.path(),
            &[
                (
                    "bmad/pack.toml",
                    "[pack]\nname=\"bmad\"\nschema=2\n[imports.gc]\nsource=\"../gascity\"\n",
                ),
                ("bmad/agents/architect/prompt.md", "architect"),
                (
                    "gascity/formulas/build-base.formula.toml",
                    "formula=\"build-base\"\n",
                ),
                (
                    "gascity/roles/pack.toml",
                    "[pack]\nname=\"gc-roles\"\nschema=2\n",
                ),
                ("gascity/roles/agents/run-operator/prompt.md", "operate"),
            ],
        );
        let camp = tempfile::tempdir().unwrap();
        let root = camp.path();
        std::fs::write(
            root.join("camp.toml"),
            "[camp]\nname=\"t\"\n[agent_defaults]\ntools=[\"Read\"]\n",
        )
        .unwrap();
        camp_core::ledger::Ledger::open(&root.join("camp.db")).unwrap();
        let base = format!("file://{}", repo.path().display());

        // 1. bmad → transitively binds `gc` to gascity (a CONTENT layer).
        run_add(root, &format!("{base}//bmad"), Some("bmad"), None, None).unwrap();
        assert!(
            root.join("imports")
                .join(camp_core::config::TRANSITIVE_DIR)
                .join("gc")
                .join("formulas")
                .is_dir(),
            "a transitive layer materializes under the transitive sentinel, \
             NOT into the binding's own dir"
        );

        // 2. the operator DIRECTLY binds `gc` to the roles pack — the override.
        run_add(
            root,
            &format!("{base}//gascity/roles"),
            Some("gc"),
            None,
            None,
        )
        .unwrap();

        let cfg = camp_core::config::CampConfig::load(&root.join("camp.toml")).unwrap();

        // The DIRECT import owns imports/gc/ — its agents resolve...
        assert_eq!(
            camp_core::pack::resolve_agent(&cfg, "gc.run-operator")
                .unwrap()
                .name,
            "gc.run-operator"
        );
        // ...and the transitive content is NOT merged into it. Under the old
        // merge-into-one-dir behavior this file existed, so this is the
        // assertion that separates OVERRIDE from MERGE.
        assert!(
            !root
                .join("imports")
                .join("gc")
                .join("formulas")
                .join("build-base.formula.toml")
                .exists(),
            "the direct import's dir must hold DIRECT content only — the \
             transitive layer lives on its own path"
        );
        // ...yet the transitive formula layer SURVIVES the override, resolved
        // by BARE name (§7.2). This is the whole point of the ruling: the 24
        // corpus formulas that `extends = [...]` gascity keep compiling.
        assert!(
            camp_core::orders::resolve_formula(&cfg, "build-base").is_ok(),
            "the transitive formula layer must survive a direct override of \
             its binding"
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
        let err = run_add(camp.path(), &url, Some("bmad"), None, None)
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
        let err = run_add(camp.path(), &url, Some("bmad"), None, None)
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
        run_add(camp.path(), &url, Some("bmad"), None, None).unwrap();
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
        run_add(camp.path(), &url, Some("bmad"), None, None).unwrap();
        let led = camp_core::ledger::Ledger::open(&camp.path().join("camp.db")).unwrap();
        let added = led
            .events_of_type(camp_core::event::EventType::ImportAdded)
            .unwrap();
        let reported = added[0].data["reported"].to_string();
        let np = &added[0].data["reported"]["nested_packs"][0];
        // Bug a (#137): the reported location is the nested pack's REAL subpath
        // in the source repo — the transitive import's subpath (`gascity`)
        // joined with the pack's path inside that layer (`roles`) — NOT the
        // top-level binding (`bmad/roles`) nor the bare layer-relative `roles`.
        assert_eq!(
            np["subpath"], "gascity/roles",
            "nested pack must report its real source subpath: {reported}"
        );
        assert_eq!(np["name"], "gc-roles", "nested pack name: {reported}");
        // The reported import source is a runnable `camp import add` argument
        // that re-imports exactly this nested pack from the same repo.
        let import_source = np["import_source"]
            .as_str()
            .unwrap_or_else(|| panic!("import_source must be a string: {reported}"));
        assert!(
            import_source.contains("//gascity/roles"),
            "import_source must point at the real subpath: {import_source}"
        );
        assert!(
            !import_source.contains("//bmad/roles"),
            "import_source must not use the top-level binding as the path: {import_source}"
        );
    }

    #[test]
    fn add_reports_nested_pack_via_a_local_import_at_its_real_on_disk_path() {
        // §7.3 / #137: a LOCAL import (not `file://` — the branch that renders
        // `import_source` from `repo_dir.join(subpath)`) must report the nested
        // pack's REAL on-disk source directory, never camp's internal
        // `.transitive` materialization copy. The reported path must exist so
        // the suggested `camp import add <path>` actually runs.
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
        // An absolute local path — is_local_source treats it as a local import
        // (no `://`), unlike the `file://` form the sibling test uses.
        let local_src = repo.path().join("bmad");
        run_add(
            camp.path(),
            local_src.to_str().unwrap(),
            Some("bmad"),
            None,
            None,
        )
        .unwrap();
        let led = camp_core::ledger::Ledger::open(&camp.path().join("camp.db")).unwrap();
        let added = led
            .events_of_type(camp_core::event::EventType::ImportAdded)
            .unwrap();
        let reported = added[0].data["reported"].to_string();
        let np = &added[0].data["reported"]["nested_packs"][0];
        assert_eq!(
            np["subpath"], "gascity/roles",
            "local nested pack must report its real source subpath: {reported}"
        );
        assert_eq!(np["name"], "gc-roles", "nested pack name: {reported}");
        let import_source = np["import_source"]
            .as_str()
            .unwrap_or_else(|| panic!("import_source must be a string: {reported}"));
        assert!(
            import_source.ends_with("gascity/roles"),
            "local import_source must be the on-disk source dir: {import_source}"
        );
        assert!(
            !import_source.contains(".transitive"),
            "local import_source must be the real source, never camp's \
             internal .transitive copy: {import_source}"
        );
        assert!(
            std::path::Path::new(import_source).is_dir(),
            "local import_source must point at a directory that actually \
             exists, so the suggested command runs: {import_source}"
        );
    }

    #[test]
    fn self_referential_transitive_does_not_report_the_pack_as_its_own_nested_pack() {
        // #139: gascity/roles imports its parent `..` (gascity), and gascity
        // re-contains roles/ — so the transitive subtree holds a copy of the
        // very pack being imported. camp must NOT report that already-bound
        // pack as an un-composed nested pack (it would tell you to import what
        // you just imported). The #137 case (a genuinely un-composed nested
        // pack) must still be reported — guarded by the sibling test.
        let repo = tempfile::tempdir().unwrap();
        testsupport::init_repo(
            repo.path(),
            &[
                ("gascity/pack.toml", "[pack]\nname=\"gascity\"\nschema=2\n"),
                (
                    "gascity/roles/pack.toml",
                    "[pack]\nname=\"gc-roles\"\nschema=2\n[imports.gc]\nsource=\"..\"\n",
                ),
                ("gascity/roles/agents/a/prompt.md", "a"),
            ],
        );
        let camp = tempfile::tempdir().unwrap();
        std::fs::write(
            camp.path().join("camp.toml"),
            "[camp]\nname=\"t\"\n[agent_defaults]\ntools=[\"Read\"]\n",
        )
        .unwrap();
        camp_core::ledger::Ledger::open(&camp.path().join("camp.db")).unwrap();
        let url = format!("file://{}//gascity/roles", repo.path().display());
        run_add(camp.path(), &url, Some("gc"), None, None).unwrap();
        let led = camp_core::ledger::Ledger::open(&camp.path().join("camp.db")).unwrap();
        let added = led
            .events_of_type(camp_core::event::EventType::ImportAdded)
            .unwrap();
        let reported = &added[0].data["reported"];
        assert_eq!(
            reported["nested_packs"].as_array().map(Vec::len),
            Some(0),
            "a pack already bound by this import must not be reported as its \
             own nested pack: {reported}"
        );
    }

    #[test]
    fn self_reference_suppression_is_insensitive_to_a_trailing_slash_source() {
        // #139 hardening: the direct import's subpath is the operator's verbatim
        // `//subpath`, so `//gascity/roles/` yields subpath "gascity/roles/"
        // while the rediscovered nested pack is "gascity/roles". The already-
        // bound comparison must canonicalize both, or the false note re-fires.
        let repo = tempfile::tempdir().unwrap();
        testsupport::init_repo(
            repo.path(),
            &[
                ("gascity/pack.toml", "[pack]\nname=\"gascity\"\nschema=2\n"),
                (
                    "gascity/roles/pack.toml",
                    "[pack]\nname=\"gc-roles\"\nschema=2\n[imports.gc]\nsource=\"..\"\n",
                ),
                ("gascity/roles/agents/a/prompt.md", "a"),
            ],
        );
        let camp = tempfile::tempdir().unwrap();
        std::fs::write(
            camp.path().join("camp.toml"),
            "[camp]\nname=\"t\"\n[agent_defaults]\ntools=[\"Read\"]\n",
        )
        .unwrap();
        camp_core::ledger::Ledger::open(&camp.path().join("camp.db")).unwrap();
        // Note the TRAILING SLASH on the subpath.
        let url = format!("file://{}//gascity/roles/", repo.path().display());
        run_add(camp.path(), &url, Some("gc"), None, None).unwrap();
        let led = camp_core::ledger::Ledger::open(&camp.path().join("camp.db")).unwrap();
        let added = led
            .events_of_type(camp_core::event::EventType::ImportAdded)
            .unwrap();
        let reported = &added[0].data["reported"];
        assert_eq!(
            reported["nested_packs"].as_array().map(Vec::len),
            Some(0),
            "a trailing-slash source must not defeat already-bound suppression: {reported}"
        );
    }

    #[test]
    fn nested_pack_note_is_runnable_and_unquoted() {
        // §7.3 / #137: the note names the un-composed nested pack, its real
        // source subpath, and a COPY-PASTEABLE `camp import add` command — with
        // no literal JSON quotes around the path (bug b) and the real subpath,
        // not the top-level binding (bug a).
        let np = NestedPack {
            source_subpath: "gascity/roles".to_owned(),
            name: "gc-roles".to_owned(),
            import_source: "https://github.com/gastownhall/gascity-packs//gascity/roles#main"
                .to_owned(),
        };
        let note = nested_pack_note(&np);
        assert!(
            note.contains("gascity/roles"),
            "note must show the real source subpath: {note}"
        );
        assert!(
            !note.contains("bmad/roles"),
            "note must not use the top-level binding as the path: {note}"
        );
        // Bug b: no bare JSON-value quote artifact like `/"roles"` in the path.
        assert!(
            !note.contains("/\"roles\""),
            "note must not emit literal JSON quotes around the path: {note}"
        );
        // A fully runnable command against the real source; the binding is the
        // one value the operator must choose, so it stays a placeholder.
        assert!(
            note.contains(
                "camp import add \
                 \"https://github.com/gastownhall/gascity-packs//gascity/roles#main\""
            ),
            "note must be a runnable command quoting the real source: {note}"
        );
        assert!(
            note.contains("--name <binding>"),
            "note must leave the binding as an operator-chosen placeholder: {note}"
        );
    }
}

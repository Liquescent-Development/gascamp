//! Keep a repo-local camp's live runtime state out of git (issue #35).
//!
//! `camp init` / `camp rig add` create a `.camp/` holding a live WAL-mode
//! SQLite ledger (`camp.db` + `-wal` + `-shm`), the daemon socket and its bind
//! lock (`campd.sock`, `campd.sock.lock`), and logs (`campd.log`) — plus the
//! per-run, per-session, and worktree scratch dirs. Left unignored, the next
//! `git add .` stages a live database and a Unix socket.
//!
//! When the camp lives inside a git repo, idempotently ensure that repo's
//! `.gitignore` covers the camp's runtime state, while keeping `camp.toml`
//! tracked: `camp.toml` is the human-authored source of truth (spec §7.1,
//! §13.4; invariant 3's "human-readable TOML"; phase-3 decision D — rigs live
//! in `camp.toml`). Outside a git repo we do nothing — we never fabricate one.

use std::collections::BTreeSet;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Runtime files inside the camp dir: the ledger and its WAL/SHM sidecars, the
/// daemon socket and its bind lock, and the daemon log (spec §7.1, §5).
const RUNTIME_FILES: &[&str] = &[
    "camp.db",
    "camp.db-wal",
    "camp.db-shm",
    "campd.sock",
    "campd.sock.lock",
    "campd.log",
];

/// Runtime directories inside the camp dir: per-run cook state, per-worker
/// capture, and camp-managed git worktrees (spec §7.1). All generated at
/// runtime — never source. (`formulas/`, human-authored, is intentionally
/// absent: it stays tracked alongside `camp.toml`.)
const RUNTIME_DIRS: &[&str] = &["runs", "sessions", "worktrees", "imports"];

/// Header line so a human reading `.gitignore` knows what the block is and why
/// `camp.toml` is deliberately excluded from it.
const HEADER: &str = "# Gas Camp runtime state (camp.toml is the tracked source of truth)";

/// If `camp_root` lives inside a git repository, idempotently ensure that
/// repo's `.gitignore` ignores the camp's live runtime state. A no-op outside
/// a git repo and on repeated runs (an already-present entry is never
/// duplicated).
pub fn ensure_camp_runtime_ignored(camp_root: &Path) -> Result<()> {
    let Some(repo_root) = enclosing_git_repo(camp_root) else {
        return Ok(());
    };
    let rel = camp_root.strip_prefix(&repo_root).with_context(|| {
        format!(
            "camp dir {} is not under its git repo {}",
            camp_root.display(),
            repo_root.display()
        )
    })?;
    let prefix = anchored_prefix(rel)?;

    let mut entries: Vec<String> = Vec::with_capacity(RUNTIME_FILES.len() + RUNTIME_DIRS.len());
    for file in RUNTIME_FILES {
        entries.push(format!("{prefix}/{file}"));
    }
    for dir in RUNTIME_DIRS {
        entries.push(format!("{prefix}/{dir}/"));
    }
    append_missing(&repo_root.join(".gitignore"), &entries)
}

/// Walk up from `start` for a `.git` entry (a directory for a normal repo, a
/// file for a worktree/submodule gitdir pointer). Returns the repo root, or
/// `None` when `start` is not inside a git repo.
fn enclosing_git_repo(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|dir| dir.join(".git").exists())
        .map(Path::to_path_buf)
}

/// The camp dir relative to the repo root as a leading-slash-anchored gitignore
/// prefix: `.camp` → `/.camp`, nested `sub/.camp` → `/sub/.camp`, and the repo
/// root itself (camp == repo) → `""` so entries anchor at the root (`/camp.db`).
/// Anchoring pins the rules to this camp, never some unrelated `.camp/` deeper
/// in the tree.
fn anchored_prefix(rel: &Path) -> Result<String> {
    if rel.as_os_str().is_empty() {
        return Ok(String::new());
    }
    let mut parts: Vec<&str> = Vec::new();
    for component in rel.components() {
        let part = component
            .as_os_str()
            .to_str()
            .with_context(|| format!("non-UTF-8 path component in {}", rel.display()))?;
        parts.push(part);
    }
    Ok(format!("/{}", parts.join("/")))
}

/// Append each entry not already present verbatim, plus the header once when
/// the block is new. Idempotent: when every entry is already present the file
/// is left byte-for-byte unchanged (no write, no duplicate).
fn append_missing(gitignore: &Path, entries: &[String]) -> Result<()> {
    let existing = match std::fs::read_to_string(gitignore) {
        Ok(contents) => contents,
        Err(err) if err.kind() == ErrorKind::NotFound => String::new(),
        Err(err) => {
            return Err(err).with_context(|| format!("cannot read {}", gitignore.display()));
        }
    };
    let present: BTreeSet<&str> = existing.lines().map(str::trim).collect();
    let header_present = present.contains(HEADER);
    let missing: Vec<String> = entries
        .iter()
        .filter(|entry| !present.contains(entry.as_str()))
        .cloned()
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    drop(present); // end the borrow of `existing` so it can back `out`

    let mut out = existing;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    if !out.is_empty() {
        // A blank line separates our block from any pre-existing content.
        out.push('\n');
    }
    if !header_present {
        out.push_str(HEADER);
        out.push('\n');
    }
    for entry in &missing {
        out.push_str(entry);
        out.push('\n');
    }
    std::fs::write(gitignore, &out).with_context(|| format!("cannot write {}", gitignore.display()))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn anchored_prefix_repo_local_camp() {
        assert_eq!(anchored_prefix(Path::new(".camp")).unwrap(), "/.camp");
    }

    #[test]
    fn anchored_prefix_nested_camp() {
        assert_eq!(
            anchored_prefix(Path::new("sub/.camp")).unwrap(),
            "/sub/.camp"
        );
    }

    #[test]
    fn anchored_prefix_camp_is_repo_root() {
        assert_eq!(anchored_prefix(Path::new("")).unwrap(), "");
    }

    #[test]
    fn no_git_repo_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        let camp_root = dir.path().join(".camp");
        std::fs::create_dir_all(&camp_root).unwrap();
        ensure_camp_runtime_ignored(&camp_root).unwrap();
        assert!(
            !dir.path().join(".gitignore").exists(),
            "no .gitignore may be fabricated outside a git repo"
        );
    }

    #[test]
    fn append_missing_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let gitignore = dir.path().join(".gitignore");
        let entries = vec!["/.camp/camp.db".to_owned(), "/.camp/runs/".to_owned()];

        append_missing(&gitignore, &entries).unwrap();
        let first = std::fs::read_to_string(&gitignore).unwrap();
        append_missing(&gitignore, &entries).unwrap();
        let second = std::fs::read_to_string(&gitignore).unwrap();

        assert_eq!(first, second, "second run must not change the file");
        assert_eq!(first.matches("/.camp/camp.db\n").count(), 1);
        assert_eq!(first.matches("/.camp/runs/\n").count(), 1);
    }

    #[test]
    fn append_missing_preserves_prior_content_and_adds_only_gaps() {
        let dir = tempfile::tempdir().unwrap();
        let gitignore = dir.path().join(".gitignore");
        std::fs::write(&gitignore, "/target\n/.camp/camp.db\n").unwrap();
        let entries = vec!["/.camp/camp.db".to_owned(), "/.camp/campd.log".to_owned()];

        append_missing(&gitignore, &entries).unwrap();
        let out = std::fs::read_to_string(&gitignore).unwrap();

        assert!(out.contains("/target\n"), "prior content preserved: {out}");
        assert_eq!(
            out.matches("/.camp/camp.db\n").count(),
            1,
            "existing entry not duplicated: {out}"
        );
        assert_eq!(out.matches("/.camp/campd.log\n").count(), 1);
    }

    #[test]
    fn imports_dir_is_a_runtime_dir() {
        assert!(RUNTIME_DIRS.contains(&"imports"), "materialized imports must be gitignored");
    }

    #[test]
    fn imports_entry_is_written_anchored_and_packs_lock_stays_tracked() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        let camp = dir.path().join(".camp");
        std::fs::create_dir_all(&camp).unwrap();
        ensure_camp_runtime_ignored(&camp).unwrap();
        let gi = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert!(gi.contains("/.camp/imports/"), "{gi}");
        assert!(!gi.contains("packs.lock"), "packs.lock stays tracked: {gi}");
    }
}

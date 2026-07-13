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

use std::path::Path;

use anyhow::{Context, Result, bail};

/// The hardened git argv, byte-for-byte: ten `-c KEY=VALUE` pairs, in order.
/// Pinned by `hardened_git_argv_is_exact`; do not reorder.
pub fn hardened_git_args() -> [&'static str; 20] {
    [
        "-c", "http.followRedirects=false",
        "-c", "protocol.allow=never",
        "-c", "protocol.https.allow=always",
        "-c", "protocol.http.allow=always",
        "-c", "protocol.ssh.allow=always",
        "-c", "protocol.git.allow=always",
        "-c", "protocol.file.allow=always",
        "-c", "core.hooksPath=/dev/null",
        "-c", "core.fsmonitor=false",
        "-c", "core.untrackedCache=false",
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
    cmd.args(hardened_git_args()).arg("clone").arg(repository).arg(dest);
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

/// Test-only fixture builder: init a git repo at `dir`, write each file
/// (creating parent dirs), then add + commit. Reused by the verb tests and
/// the end-to-end acceptance test.
#[cfg(test)]
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
            .args(["-c", "user.email=t@t", "-c", "user.name=t", "-c", "commit.gpgsign=false"])
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
        assert_eq!(hardened_git_args(), [
            "-c", "http.followRedirects=false",
            "-c", "protocol.allow=never",
            "-c", "protocol.https.allow=always",
            "-c", "protocol.http.allow=always",
            "-c", "protocol.ssh.allow=always",
            "-c", "protocol.git.allow=always",
            "-c", "protocol.file.allow=always",
            "-c", "core.hooksPath=/dev/null",
            "-c", "core.fsmonitor=false",
            "-c", "core.untrackedCache=false",
        ]);
    }

    #[test]
    fn clone_and_resolve_a_file_repo() {
        let src = tempfile::tempdir().unwrap();
        testsupport::init_repo(src.path(), &[("pack.toml", "[pack]\nname = \"x\"\nschema = 2\n")]);
        let url = format!("file://{}", src.path().display());
        let sha = resolve_commit(&url, Some("HEAD")).unwrap();
        assert_eq!(sha.len(), 40, "resolved a full sha: {sha}");
        let dest = tempfile::tempdir().unwrap();
        git_clone(&url, &dest.path().join("clone")).unwrap();
        assert!(dest.path().join("clone/pack.toml").exists());
    }
}
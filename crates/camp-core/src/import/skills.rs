//! Skills install (umbrella §5.3): copy a pack's `skills/` into a session
//! worktree at `<worktree>/.claude/skills/<skill>/`, and make `<worktree>/.claude/`
//! self-ignoring so `git add -A` never stages installed skills. Refuses LOUDLY
//! if the worktree already tracks `.claude/.gitignore`, or a tracked file
//! collides with a skill — camp never overwrites the operator's committed
//! `.claude/` state.
//!
//! These are LOCAL git invocations against the worktree's own repo (no
//! untrusted URL), so plain `git` is fine — the hardened subprocess is for
//! network fetches.

use std::path::Path;

use crate::error::CoreError;

/// Install a pack's skills/ into a session worktree (umbrella §5.3):
///   `<worktree>/.claude/skills/<skill>/...`  from `<pack_dir>/skills/`
///   `<worktree>/.claude/.gitignore` = `"*\n"` (self-ignoring)
/// Refuses LOUDLY if the worktree TRACKS `.claude/.gitignore`, or a tracked
/// file collides with a skill. Returns the number of skills installed.
/// No `<pack_dir>/skills/` → `Ok(0)`.
pub fn install_skills(pack_dir: &Path, worktree: &Path) -> Result<usize, CoreError> {
    let skills_src = pack_dir.join("skills");
    if !skills_src.is_dir() {
        return Ok(0);
    }
    let dot_claude = worktree.join(".claude");
    let skills_dest = dot_claude.join("skills");

    // Refuse if the worktree already tracks .claude/.gitignore — the
    // operator committed their own .claude/ state; camp must not overwrite it.
    if tracked(worktree, &dot_claude.join(".gitignore"))? {
        return Err(CoreError::Import {
            binding: worktree.display().to_string(),
            reason: "worktree tracks .claude/.gitignore — camp will not overwrite committed \
                     .claude/ state; untrack it or remove the pack's skills/"
                .to_owned(),
        });
    }

    let mut count = 0usize;
    let entries = std::fs::read_dir(&skills_src).map_err(|e| CoreError::Import {
        binding: skills_src.display().to_string(),
        reason: format!("cannot read skills/: {e}"),
    })?;
    let mut skill_dirs: Vec<std::path::PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    skill_dirs.sort();
    for skill in skill_dirs {
        let name = skill.file_name().and_then(|n| n.to_str()).unwrap_or("");
        // Refuse a tracked collision with this skill.
        if tracked(worktree, &skills_dest.join(name))? {
            return Err(CoreError::Import {
                binding: worktree.display().to_string(),
                reason: format!(
                    "worktree tracks .claude/skills/{name} — camp will not overwrite committed \
                     skills; untrack it or remove the pack's skills/{name}"
                ),
            });
        }
        copy_tree(&skill, &skills_dest.join(name))?;
        count += 1;
    }

    // Self-ignore .claude/ so installed skills never get staged.
    std::fs::create_dir_all(&dot_claude).map_err(|e| CoreError::Import {
        binding: worktree.display().to_string(),
        reason: format!("cannot create {}: {e}", dot_claude.display()),
    })?;
    std::fs::write(dot_claude.join(".gitignore"), "*\n").map_err(|e| CoreError::Import {
        binding: worktree.display().to_string(),
        reason: format!("cannot write .claude/.gitignore: {e}"),
    })?;
    Ok(count)
}

/// Is `path` tracked by the worktree's git? `git ls-files --error-unmatch`
/// exits 0 when tracked, non-zero otherwise.
fn tracked(worktree: &Path, path: &Path) -> Result<bool, CoreError> {
    let rel = path.strip_prefix(worktree).map_err(|_| CoreError::Import {
        binding: worktree.display().to_string(),
        reason: format!("{} is not inside the worktree", path.display()),
    })?;
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["ls-files", "--error-unmatch"])
        .arg(rel)
        .output()
        .map_err(|e| CoreError::Import {
            binding: worktree.display().to_string(),
            reason: format!("cannot run git ls-files: {e}"),
        })?;
    Ok(out.status.success())
}

/// Recursive copy of regular files/dirs (skills are already-materialized
/// content — no symlinks expected; a symlink here is an error, not a copy).
fn copy_tree(src: &Path, dest: &Path) -> Result<(), CoreError> {
    std::fs::create_dir_all(dest).map_err(|e| CoreError::Import {
        binding: src.display().to_string(),
        reason: format!("cannot create {}: {e}", dest.display()),
    })?;
    for entry in std::fs::read_dir(src).map_err(|e| CoreError::Import {
        binding: src.display().to_string(),
        reason: format!("cannot read {}: {e}", src.display()),
    })? {
        let entry = entry.map_err(|e| CoreError::Import {
            binding: src.display().to_string(),
            reason: format!("cannot read entry in {}: {e}", src.display()),
        })?;
        let path = entry.path();
        let to = dest.join(entry.file_name());
        let meta = std::fs::symlink_metadata(&path).map_err(|e| CoreError::Import {
            binding: src.display().to_string(),
            reason: format!("cannot stat {}: {e}", path.display()),
        })?;
        if meta.is_symlink() {
            return Err(CoreError::Import {
                binding: src.display().to_string(),
                reason: format!("symlinks are not supported in skills/: {}", path.display()),
            });
        } else if meta.is_dir() {
            copy_tree(&path, &to)?;
        } else if meta.is_file() {
            std::fs::copy(&path, &to).map_err(|e| CoreError::Import {
                binding: src.display().to_string(),
                reason: format!("cannot copy {}: {e}", path.display()),
            })?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(dir)
                .args([
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "user.name=t",
                    "-c",
                    "commit.gpgsign=false",
                    // Neutralize the operator's global gitignore (e.g. one that
                    // excludes .claude/) so the test repo's staging is deterministic.
                    "-c",
                    "core.excludesfile=",
                ])
                .args(args)
                .status()
                .unwrap()
                .success(),
            "git {args:?}"
        );
    }
    fn pack_with_skill(root: &Path) -> std::path::PathBuf {
        let p = root.join("pack");
        std::fs::create_dir_all(p.join("skills/bmad-create-architecture")).unwrap();
        std::fs::write(
            p.join("skills/bmad-create-architecture/SKILL.md"),
            "# skill",
        )
        .unwrap();
        p
    }
    #[test]
    fn installed_skills_are_self_ignored_after_add() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        git(&wt, &["init", "-q"]);
        std::fs::write(wt.join("file.txt"), "work").unwrap();
        let n = install_skills(&pack_with_skill(dir.path()), &wt).unwrap();
        assert_eq!(n, 1);
        assert!(
            wt.join(".claude/skills/bmad-create-architecture/SKILL.md")
                .exists()
        );
        assert_eq!(
            std::fs::read_to_string(wt.join(".claude/.gitignore")).unwrap(),
            "*\n"
        );
        git(&wt, &["add", "-A"]);
        let out = Command::new("git")
            .arg("-C")
            .arg(&wt)
            .args(["-c", "core.excludesfile=", "status", "--porcelain"])
            .output()
            .unwrap();
        let s = String::from_utf8(out.stdout).unwrap();
        assert!(
            !s.contains(".claude/"),
            "nothing under .claude/ staged: {s:?}"
        );
        assert!(s.contains("file.txt"), "real work still staged: {s:?}");
    }
    #[test]
    fn tracked_dot_claude_gitignore_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("wt");
        std::fs::create_dir_all(wt.join(".claude")).unwrap();
        git(&wt, &["init", "-q"]);
        std::fs::write(wt.join(".claude/.gitignore"), "custom\n").unwrap();
        git(&wt, &["add", "-A"]);
        git(&wt, &["commit", "-q", "-m", "track"]);
        assert!(install_skills(&pack_with_skill(dir.path()), &wt).is_err());
    }
}

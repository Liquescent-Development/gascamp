//! Materialization (component §6/§7.4): copy a pack subpath tree out of a
//! checked-out repo into the materialized destination, dereferencing
//! symlinks so the result is plain bytes camp can vouch for. A symlink whose
//! canonical target escapes the repo root, or is dangling, is a hard error
//! (component §6/§7.4). `.git` is skipped.
//!
//! Dereferencing is the security property: a symlink is only followed if
//! its resolved target lives inside the repo (so a pack cannot reach
//! outside its declared tree), and the result is a regular file — the
//! materialized tree has zero symlinks, so no link's target can change
//! after materialization.

use std::path::Path;

use crate::error::CoreError;

/// Copy `src_subtree` (a pack subpath inside a checked-out repo at
/// `repo_root`) into `dest`, dereferencing symlinks. A symlink target
/// escaping `repo_root`, or dangling, is a hard error. Skips `.git`.
pub fn materialize_tree(
    repo_root: &Path,
    src_subtree: &Path,
    dest: &Path,
) -> Result<(), CoreError> {
    let repo_canon = repo_root
        .canonicalize()
        .map_err(|e| import_err(repo_root, format!("cannot canonicalize repo root: {e}")))?;
    std::fs::create_dir_all(dest)
        .map_err(|e| import_err(dest, format!("cannot create {}: {e}", dest.display())))?;
    copy_into(&repo_canon, src_subtree, dest)
}

fn copy_into(repo_canon: &Path, src: &Path, dest: &Path) -> Result<(), CoreError> {
    for entry in std::fs::read_dir(src)
        .map_err(|e| import_err(src, format!("cannot read {}: {e}", src.display())))?
    {
        let entry = entry
            .map_err(|e| import_err(src, format!("cannot read entry in {}: {e}", src.display())))?;
        let name = entry.file_name();
        if name == ".git" {
            continue;
        }
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path)
            .map_err(|e| import_err(&path, format!("cannot stat {}: {e}", path.display())))?;
        let to = dest.join(&name);
        if meta.is_symlink() {
            // Dereference: canonicalize resolves `..` and follows the link.
            // A nonexistent target errors here → dangling.
            let canon = std::fs::canonicalize(&path)
                .map_err(|_| import_err(&path, format!("dangling symlink: {}", path.display())))?;
            // The resolved target must stay inside the repo.
            if !canon.starts_with(repo_canon) {
                return Err(import_err(
                    &path,
                    format!("symlink {} escapes the repo root", path.display()),
                ));
            }
            let target_meta = std::fs::metadata(&canon).map_err(|e| {
                import_err(
                    &path,
                    format!("cannot read symlink target {}: {e}", canon.display()),
                )
            })?;
            if target_meta.is_dir() {
                std::fs::create_dir_all(&to)
                    .map_err(|e| import_err(&to, format!("cannot create {}: {e}", to.display())))?;
                copy_into(repo_canon, &canon, &to)?;
            } else {
                std::fs::copy(&canon, &to).map_err(|e| {
                    import_err(
                        &path,
                        format!("cannot copy symlink target {}: {e}", canon.display()),
                    )
                })?;
            }
        } else if meta.is_dir() {
            std::fs::create_dir_all(&to)
                .map_err(|e| import_err(&to, format!("cannot create {}: {e}", to.display())))?;
            copy_into(repo_canon, &path, &to)?;
        } else if meta.is_file() {
            std::fs::copy(&path, &to)
                .map_err(|e| import_err(&path, format!("cannot copy {}: {e}", path.display())))?;
        }
    }
    Ok(())
}

fn import_err(path: &Path, reason: String) -> CoreError {
    CoreError::Import {
        binding: path.display().to_string(),
        reason,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn dereferences_symlink_inside_repo_to_a_regular_file() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join("shared")).unwrap();
        std::fs::write(repo.path().join("shared/f.toml"), b"formula = \"x\"\n").unwrap();
        let pack = repo.path().join("packs/p/formulas");
        std::fs::create_dir_all(&pack).unwrap();
        std::os::unix::fs::symlink("../../../shared/f.toml", pack.join("g.toml")).unwrap();
        let dest = tempfile::tempdir().unwrap();
        materialize_tree(
            repo.path(),
            &repo.path().join("packs/p"),
            &dest.path().join("out"),
        )
        .unwrap();
        let out = dest.path().join("out/formulas/g.toml");
        assert!(out.is_file() && !out.is_symlink());
        assert_eq!(std::fs::read(&out).unwrap(), b"formula = \"x\"\n");
    }
    #[test]
    fn symlink_escaping_repo_root_is_hard_error() {
        let repo = tempfile::tempdir().unwrap();
        let pack = repo.path().join("p");
        std::fs::create_dir_all(&pack).unwrap();
        std::os::unix::fs::symlink("/etc/hosts", pack.join("evil")).unwrap();
        let dest = tempfile::tempdir().unwrap();
        let err = materialize_tree(repo.path(), &pack, &dest.path().join("out")).unwrap_err();
        assert!(
            err.to_string().to_lowercase().contains("escape") || err.to_string().contains("repo"),
            "{err}"
        );
    }
    #[test]
    fn dangling_symlink_is_hard_error() {
        let repo = tempfile::tempdir().unwrap();
        let pack = repo.path().join("p");
        std::fs::create_dir_all(&pack).unwrap();
        std::os::unix::fs::symlink("./nope.toml", pack.join("g.toml")).unwrap();
        let dest = tempfile::tempdir().unwrap();
        assert!(materialize_tree(repo.path(), &pack, &dest.path().join("out")).is_err());
    }
}

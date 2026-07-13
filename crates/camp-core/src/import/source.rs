//! Source grammar (component spec §4/§5): a pack import source normalizes to
//! a repository, an optional subpath, and an optional reference. A local path
//! (a directory on disk) is a first-class source — phase-1 tests clone `file://`
//! fixtures, never the network.
//!
//! Grammar:
//! - **local path** (`./`, `../`, `/abs`, bare relative) → `is_local_path`,
//!   `repository` verbatim; a `version` on a local path is rejected (a local
//!   tree has no ref to pin).
//! - **`<repo-url>//<subpath>#<ref>`** — the go-getter subdir marker (`//`,
//!   distinct from the scheme's `://`) plus an optional `#ref`.
//! - **GitHub tree URL** `.../tree/{ref}[/{path}]` — the convenience form,
//!   rewritten to repo + subpath + ref.
//! - **transports** `https | http | ssh | git@ | file` (anything else, e.g.
//!   `ext::`, is rejected — `ext::` runs arbitrary commands and is the hole
//!   the allowlist closes).
//!
//! The ref comes from at most one of {tree-url, `#ref`, `version`}; two that
//! disagree → error. `file://` MUST be accepted (phase-1 fixtures use it).

use crate::error::CoreError;

/// A normalized import source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub repository: String,
    pub subpath: Option<String>,
    pub reference: Option<String>,
    pub is_local_path: bool,
}

/// Is this source a LOCAL filesystem path (as opposed to a git-backed
/// remote)? Starts with `.` or `/`, or has no scheme / `git@` / `//` / `::`
/// (the `::` exclusion sends git's `ext::` transport to the remote path,
/// where the allowlist rejects it).
///
/// The ONE definition of "local" — `normalize` classifies a CLI source with
/// it, and `ImportDecl::is_local` classifies an already-declared
/// `[imports.<binding>]` with it, so `camp import add` and every resolver
/// can never disagree about which imports are layered in place (§5).
pub fn is_local_source(s: &str) -> bool {
    let s = s.trim();
    s.starts_with('.')
        || s.starts_with('/')
        || (!s.contains("://") && !s.starts_with("git@") && !s.contains("//") && !s.contains("::"))
}

/// Normalize a raw source string plus an optional `version` (from
/// `ImportDecl.version`) into a `Source`. Errors are `CoreError::Import`
/// naming the source as the binding for actionable messages.
pub fn normalize(source: &str, version: Option<&str>) -> Result<Source, CoreError> {
    let s = source.trim();
    if s.is_empty() {
        return Err(CoreError::Import {
            binding: source.to_owned(),
            reason: "empty source".to_owned(),
        });
    }
    let is_local_path = is_local_source(s);
    if is_local_path {
        if let Some(v) = version {
            return Err(CoreError::Import {
                binding: source.to_owned(),
                reason: format!(
                    "a local path ({s:?}) cannot pin a version/ref — remove version {v:?}"
                ),
            });
        }
        return Ok(Source {
            repository: s.to_owned(),
            subpath: None,
            reference: None,
            is_local_path: true,
        });
    }

    // Remote. Strip the rightmost `#ref` first (go-getter convention).
    let (url_part, hash_ref) = match s.rfind('#') {
        Some(i) => (&s[..i], Some(s[i + 1..].to_owned())),
        None => (s, None),
    };

    // GitHub tree URL convenience form: `.../tree/{ref}[/{path}]`.
    let (repo, subpath, tree_ref) = if let Some(tree_idx) = url_part.find("/tree/") {
        let repo = &url_part[..tree_idx];
        let after = &url_part[tree_idx + "/tree/".len()..];
        let (ref_part, path_part) = match after.find('/') {
            Some(i) => (&after[..i], Some(&after[i + 1..])),
            None => (after, None),
        };
        (
            repo.to_owned(),
            path_part.map(|p| p.to_owned()),
            Some(ref_part.to_owned()),
        )
    } else {
        // Generic `<repo>//<subpath>`: the first `//` that is NOT the
        // scheme's `://`.
        let (repo, subpath) = split_subdir_marker(url_part);
        (repo.to_owned(), subpath.map(|p| p.to_owned()), None)
    };

    // Reconcile the ref among {tree-url, #ref, version}: at most one may
    // differ (two equal is idempotent, not a conflict).
    let reference = reconcile_refs(tree_ref, hash_ref, version.map(|v| v.to_owned()), source)?;

    // Validate the transport allowlist.
    validate_transport(&repo, source)?;

    // Neither the repository nor the ref may LOOK like an option. Both reach
    // git as bare positionals (`git ls-remote <repo> <ref>`), and the contract
    // camp's hardening rests on is a PINNED argv — a value that git could parse
    // as a flag breaks that contract by construction. Refuse it at the boundary
    // rather than depend on git's transport rules to save us downstream.
    for (what, value) in [
        ("repository", repo.as_str()),
        ("ref", reference.as_deref().unwrap_or("")),
    ] {
        if value.starts_with('-') {
            return Err(CoreError::Import {
                binding: source.to_owned(),
                reason: format!(
                    "{what} {value:?} begins with '-' — it would reach git as an option, \
                     not a value"
                ),
            });
        }
    }

    Ok(Source {
        repository: repo,
        subpath,
        reference,
        is_local_path: false,
    })
}

/// Split `<repo>//<subpath>` at the first `//` that is not the scheme's
/// `://` separator. No `//` after the scheme → no subpath.
fn split_subdir_marker(s: &str) -> (&str, Option<&str>) {
    let scheme_end = s.find("://").map(|i| i + "://".len()).unwrap_or(0);
    match s[scheme_end..].find("//") {
        Some(rel) => {
            let marker = scheme_end + rel;
            (&s[..marker], Some(&s[marker + 2..]))
        }
        None => (s, None),
    }
}

/// Reconcile up to three ref sources into one. Empty → None; one → that;
/// several → all must agree or it is a conflict.
fn reconcile_refs(
    tree_ref: Option<String>,
    hash_ref: Option<String>,
    version_ref: Option<String>,
    source: &str,
) -> Result<Option<String>, CoreError> {
    let present: Vec<String> = [tree_ref, hash_ref, version_ref]
        .into_iter()
        .flatten()
        .collect();
    match present.len() {
        0 => Ok(None),
        1 => Ok(present.into_iter().next()),
        _ => {
            let first = &present[0];
            if present.iter().all(|r| r == first) {
                Ok(Some(first.clone()))
            } else {
                Err(CoreError::Import {
                    binding: source.to_owned(),
                    reason: format!(
                        "conflicting refs {present:?} — supply the ref at most once (tree-url, #ref, or version)"
                    ),
                })
            }
        }
    }
}

/// Allowed transports (component §11): `https`, `http`, `ssh`, `file`, and
/// the `git@` ssh shorthand. Anything else (notably `ext::`) is rejected.
fn validate_transport(repo: &str, source: &str) -> Result<(), CoreError> {
    if repo.starts_with("git@") {
        return Ok(());
    }
    let scheme = repo.split("://").next().unwrap_or("");
    match scheme {
        "https" | "http" | "ssh" | "file" => Ok(()),
        other => Err(CoreError::Import {
            binding: source.to_owned(),
            reason: format!(
                "unsupported transport {other:?} in {repo:?}; allowed: https, http, ssh, git@, file"
            ),
        }),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// The ref and the repository reach git as bare positionals. camp's
    /// hardening contract is a PINNED argv, so a value that git could parse as
    /// an option is refused at the boundary — not left to git's transport rules
    /// to catch downstream.
    #[test]
    fn a_ref_or_repo_that_looks_like_an_option_is_refused() {
        let err = normalize(
            "https://example.com/repo",
            Some("--upload-pack=touch /tmp/pwn"),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("begins with '-'"), "got {err}");

        let err = normalize("https://example.com/repo#--upload-pack=x", None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("begins with '-'"), "got {err}");

        // A legitimate ref that merely CONTAINS a dash is untouched.
        let s = normalize("https://example.com/repo", Some("release-1.2")).unwrap();
        assert_eq!(s.reference.as_deref(), Some("release-1.2"));
    }

    #[test]
    fn generic_form_splits_repo_subpath_ref() {
        let s = normalize("git@github.com:org/repo.git//topo#v1.0", None).unwrap();
        assert_eq!(s.repository, "git@github.com:org/repo.git");
        assert_eq!(s.subpath.as_deref(), Some("topo"));
        assert_eq!(s.reference.as_deref(), Some("v1.0"));
        assert!(!s.is_local_path);
    }
    #[test]
    fn github_tree_url_is_the_convenience_form() {
        let s = normalize(
            "https://github.com/gastownhall/gascity-packs/tree/main/bmad",
            None,
        )
        .unwrap();
        assert_eq!(s.repository, "https://github.com/gastownhall/gascity-packs");
        assert_eq!(s.subpath.as_deref(), Some("bmad"));
        assert_eq!(s.reference.as_deref(), Some("main"));
    }
    #[test]
    fn file_url_with_subpath_and_ref_is_accepted() {
        let s = normalize("file:///tmp/repo//bmad#main", None).unwrap();
        assert_eq!(s.repository, "file:///tmp/repo");
        assert_eq!(s.subpath.as_deref(), Some("bmad"));
        assert_eq!(s.reference.as_deref(), Some("main"));
    }
    #[test]
    fn local_path_carries_no_ref_and_rejects_version() {
        let s = normalize("../packs/house", None).unwrap();
        assert!(
            s.is_local_path
                && s.repository == "../packs/house"
                && s.subpath.is_none()
                && s.reference.is_none()
        );
        assert!(normalize("../packs/house", Some("v1")).is_err());
    }
    #[test]
    fn version_supplies_the_ref_when_the_source_omits_it() {
        assert_eq!(
            normalize("https://github.com/o/r", Some("sha:abc"))
                .unwrap()
                .reference
                .as_deref(),
            Some("sha:abc")
        );
    }
    #[test]
    fn conflicting_refs_are_an_error() {
        assert!(
            normalize("https://github.com/o/r//p#v1", Some("v2"))
                .unwrap_err()
                .to_string()
                .contains("ref")
        );
    }
    #[test]
    fn ext_transport_is_rejected() {
        assert!(normalize("ext::sh -c whoami", None).is_err());
    }
}

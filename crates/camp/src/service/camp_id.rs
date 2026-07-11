//! `<camp-id>`: the stable, collision-free, human-readable slug that names a
//! camp's unit (design §5). It is the whole of the launchd label
//! `com.gascamp.campd.<camp-id>` and the systemd unit name
//! `campd-<camp-id>.service`, so its charset must be safe in both: lowercase
//! ASCII alphanumerics and '-'. Nothing else.

use std::path::Path;

use anyhow::{Context, Result, bail};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CampId(String);

impl CampId {
    /// Read an id back out of an installed unit's filename. The charset is
    /// VALIDATED: a file we did not write must never become a `launchctl`
    /// argument.
    pub fn from_slug(slug: &str) -> Result<CampId> {
        let valid = !slug.is_empty()
            && slug
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if !valid {
            bail!("{slug:?} is not a camp id (lowercase alphanumerics and '-' only)");
        }
        Ok(CampId(slug.to_owned()))
    }

    /// The id of the camp rooted at `root`, which must EXIST: the path is
    /// canonicalized first, so `--camp .camp`, an absolute path, and a
    /// symlinked path all name the SAME unit.
    pub fn for_camp(root: &Path) -> Result<CampId> {
        let absolute = std::fs::canonicalize(root)
            .with_context(|| format!("resolving the camp path {}", root.display()))?;
        Ok(CampId::from_absolute(&absolute))
    }

    /// PURE: absolute path → id. `<human slug>-<8 hex>`: human-readable (read
    /// the label, know the camp) AND collision-free (every repo-local camp
    /// would otherwise be "camp"). The digest is UUIDv5 — a SPEC'D SHA-1 over
    /// the path, stable across runs, hosts and releases. (std's DefaultHasher
    /// is documented as unstable across Rust versions; a label that changes
    /// under the operator's feet would orphan their unit.)
    pub fn from_absolute(absolute: &Path) -> CampId {
        use std::os::unix::ffi::OsStrExt as _;
        let digest =
            uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, absolute.as_os_str().as_bytes());
        let hash: String = digest.simple().to_string().chars().take(8).collect();
        CampId(format!("{}-{hash}", human_slug(absolute)))
    }
}

/// The human half of the id. A repo-local `.camp` is named after its repo
/// directory; an explicit camp dir (`~/camps/dev`) after itself — the same
/// rule `camp init` uses to name a camp (`cmd/init.rs::camp_name`). Munged to
/// `[a-z0-9-]` (a launchd label and a systemd unit name share no wider
/// charset) and capped, because the hash — not the slug — carries uniqueness.
fn human_slug(absolute: &Path) -> String {
    let own = absolute.file_name().and_then(|name| name.to_str());
    let source = if own == Some(".camp") {
        absolute
            .parent()
            .and_then(|parent| parent.file_name())
            .and_then(|name| name.to_str())
    } else {
        own
    };
    let mut slug = String::new();
    for c in source.unwrap_or("camp").chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let capped: String = slug.trim_matches('-').chars().take(32).collect();
    let capped = capped.trim_end_matches('-').to_owned();
    if capped.is_empty() {
        "camp".to_owned()
    } else {
        capped
    }
}

impl std::fmt::Display for CampId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn from_slug_accepts_a_camp_id_and_rejects_anything_else() {
        assert_eq!(
            CampId::from_slug("dev-f9481b53").unwrap().to_string(),
            "dev-f9481b53"
        );
        // The id becomes a launchd LABEL and a systemd UNIT NAME. A file we
        // did not write must never become a launchctl argument.
        for bad in ["", "Dev", "dev.1", "dev/1", "../etc", "dev_1", "dev 1"] {
            assert!(
                CampId::from_slug(bad).is_err(),
                "{bad:?} must not parse as a camp id"
            );
        }
    }

    /// The id is STABLE (a launchd label must not change under the operator's
    /// feet), HUMAN-READABLE (you can read a label and know the camp), and
    /// COLLISION-FREE (every repo's `.camp` would otherwise be "camp").
    #[test]
    fn the_id_is_stable_human_readable_and_collision_free() {
        // Pinned: UUIDv5 (a spec'd SHA-1 digest) over the absolute path, not
        // std's DefaultHasher (documented as unstable across releases).
        assert_eq!(
            CampId::from_absolute(Path::new("/Users/x/camps/dev/.camp")).to_string(),
            "dev-f9481b53"
        );
        // Same path, same id, run after run.
        assert_eq!(
            CampId::from_absolute(Path::new("/Users/x/camps/dev/.camp")),
            CampId::from_absolute(Path::new("/Users/x/camps/dev/.camp"))
        );
        // Two repos, each with a `.camp`: same human half, different ids.
        let a = CampId::from_absolute(Path::new("/a/proj/.camp"));
        let b = CampId::from_absolute(Path::new("/b/proj/.camp"));
        assert_eq!(a.to_string(), "proj-6abb39d7");
        assert_eq!(b.to_string(), "proj-cdbb9b7f");
        assert_ne!(a, b, "two camps must never share a unit");
    }

    /// The id becomes a launchd label and a systemd unit name: whatever the
    /// directory is called, the slug stays `[a-z0-9-]`.
    #[test]
    fn the_human_half_is_munged_to_a_safe_slug() {
        let id = CampId::from_absolute(Path::new("/tmp/My Camp & Co/.camp"));
        assert_eq!(id.to_string(), "my-camp-co-31e4385a");
        // And it round-trips through the validating parser.
        assert_eq!(CampId::from_slug(&id.to_string()).unwrap(), id);
    }

    /// An explicit camp dir (`~/camps/dev`) is named after ITSELF; a repo-local
    /// `.camp` after its repo — the same rule `camp init` uses to name a camp.
    #[test]
    fn an_explicit_camp_dir_is_named_after_itself() {
        assert!(
            CampId::from_absolute(Path::new("/Users/x/camps/dev"))
                .to_string()
                .starts_with("dev-")
        );
    }

    /// `for_camp` canonicalizes: a relative path, an absolute path and a
    /// symlinked path to the same camp must name the SAME unit.
    #[test]
    fn for_camp_canonicalizes_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".camp");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        let direct = CampId::for_camp(&root).unwrap();
        let indirect = CampId::for_camp(&dir.path().join("sub").join("..").join(".camp")).unwrap();
        assert_eq!(direct, indirect);
        assert!(
            CampId::for_camp(&dir.path().join("nope")).is_err(),
            "a camp that does not exist is a loud error, not a fabricated id"
        );
    }
}

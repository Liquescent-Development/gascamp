//! compat §6.3 — installing the gc/bd shims into `.camp/bin`.
//!
//! Each shim is a two-line `#!/bin/sh` script that `exec`s camp's OWN absolute
//! binary as `camp gc-shim …` / `camp bd-shim …`. The absolute path (never a
//! bare `exec camp`) is the §6.3 requirement: the worker's PATH is prepended
//! with `.camp/bin`, and if the shim re-invoked `camp` by name it would find
//! ITSELF (`.camp/bin` has no `camp`), or some unrelated `camp` on PATH.

use std::path::Path;

use anyhow::{Context, Result};

/// The one place the shim's exact bytes are defined. `exec` replaces the sh
/// process so the worker sees camp's own exit code, and `"$@"` forwards gc/bd's
/// argv verbatim (camp's clap never re-parses it — see the `trailing_var_arg`
/// variants in main.rs).
fn shim_script(camp_exe: &Path, subcommand: &str) -> String {
    format!(
        "#!/bin/sh\nexec {} {subcommand} \"$@\"\n",
        camp_exe.display()
    )
}

/// Write `<camp_root>/bin/gc` and `<camp_root>/bin/bd`, each an absolute-path
/// `sh` translator, executable (0755 on unix). Idempotent: overwrites.
pub fn write_shims(camp_root: &Path, camp_exe: &Path) -> Result<()> {
    let bin = camp_root.join("bin");
    std::fs::create_dir_all(&bin)
        .with_context(|| format!("cannot create shim bindir {}", bin.display()))?;
    for (name, subcommand) in [("gc", "gc-shim"), ("bd", "bd-shim")] {
        let path = bin.join(name);
        std::fs::write(&path, shim_script(camp_exe, subcommand))
            .with_context(|| format!("cannot write shim {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .with_context(|| format!("cannot chmod shim {}", path.display()))?;
        }
    }
    Ok(())
}

/// Prepend `<camp_root>/bin` to an existing PATH (or stand alone when there is
/// none). The worker child gets this as its `PATH`, so `gc`/`bd` resolve to the
/// shims FIRST — but only for campd-dispatched workers (§6.3: attended sessions
/// get no shims).
pub fn prepend_bin_path(camp_root: &Path, existing: Option<&str>) -> String {
    let bin = format!("{}/bin", camp_root.display());
    match existing {
        Some(existing) if !existing.is_empty() => format!("{bin}:{existing}"),
        _ => bin,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn shims_embed_the_absolute_camp_path_not_a_bare_name() {
        let dir = tempfile::tempdir().unwrap();
        write_shims(dir.path(), Path::new("/opt/camp/bin/camp")).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("bin/gc")).unwrap(),
            "#!/bin/sh\nexec /opt/camp/bin/camp gc-shim \"$@\"\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("bin/bd")).unwrap(),
            "#!/bin/sh\nexec /opt/camp/bin/camp bd-shim \"$@\"\n"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(dir.path().join("bin/gc"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o111,
                0o111
            );
        }
    }

    #[test]
    fn prepend_bin_path_puts_camp_bin_first() {
        let dir = tempfile::tempdir().unwrap();
        let p = prepend_bin_path(dir.path(), Some("/usr/bin:/bin"));
        assert!(
            p.starts_with(&format!("{}/bin:", dir.path().display()))
                && p.ends_with("/usr/bin:/bin"),
            "{p}"
        );
    }

    #[test]
    fn prepend_bin_path_stands_alone_without_an_existing_path() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            prepend_bin_path(dir.path(), None),
            format!("{}/bin", dir.path().display())
        );
    }
}

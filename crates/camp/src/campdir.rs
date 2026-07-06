//! Camp directory resolution (spec §7.1): `--camp` flag, then `$CAMP_DIR`,
//! then walking up from the cwd looking for `.camp/`. No camp = hard error.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

pub struct CampDir {
    pub root: PathBuf,
}

impl CampDir {
    pub fn db_path(&self) -> PathBuf {
        self.root.join("camp.db")
    }

    pub fn resolve(flag: Option<&Path>) -> Result<CampDir> {
        if let Some(dir) = flag {
            return Self::at(dir);
        }
        if let Ok(env_dir) = std::env::var("CAMP_DIR") {
            return Self::at(Path::new(&env_dir));
        }
        let cwd = std::env::current_dir().context("cannot determine current directory")?;
        for dir in cwd.ancestors() {
            let candidate = dir.join(".camp");
            if candidate.join("camp.toml").exists() {
                return Ok(CampDir { root: candidate });
            }
        }
        bail!("no camp found; run camp init");
    }

    fn at(dir: &Path) -> Result<CampDir> {
        if dir.join("camp.toml").exists() {
            Ok(CampDir {
                root: dir.to_path_buf(),
            })
        } else {
            bail!(
                "{} is not a camp (no camp.toml); run camp init",
                dir.display()
            );
        }
    }
}

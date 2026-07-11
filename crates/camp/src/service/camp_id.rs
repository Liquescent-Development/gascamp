//! `<camp-id>`: the stable, collision-free, human-readable slug that names a
//! camp's unit (design §5). It is the whole of the launchd label
//! `com.gascamp.campd.<camp-id>` and the systemd unit name
//! `campd-<camp-id>.service`, so its charset must be safe in both: lowercase
//! ASCII alphanumerics and '-'. Nothing else.

use anyhow::{Result, bail};

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
}

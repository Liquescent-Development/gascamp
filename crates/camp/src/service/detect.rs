//! Environment detection (design §6.2 / §9): is there a usable HOST service
//! manager? Behind the `HostProbe` port, so macOS, a live systemd `--user`,
//! and a container are each just a probe — and each is a unit test.

use std::ffi::OsStr;

use super::runner::CommandRunner;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Manager {
    Launchd,
    Systemd,
}

pub trait HostProbe {
    /// `std::env::consts::OS` in production ("macos", "linux", …).
    fn os(&self) -> &str;
    fn env(&self, key: &str) -> Option<String>;
    /// Does the user's systemd manager ANSWER? (`systemctl --user
    /// show-environment` exits 0 iff the user bus / user manager is live.)
    /// This is a boolean question about the environment: no systemctl, no
    /// session and no user manager all mean the same "no". A probe, not a
    /// swallowed error.
    fn systemd_user_responds(&self) -> bool;
}

pub struct SystemProbe<'a> {
    runner: &'a dyn CommandRunner,
}

impl<'a> SystemProbe<'a> {
    pub fn new(runner: &'a dyn CommandRunner) -> SystemProbe<'a> {
        SystemProbe { runner }
    }
}

impl HostProbe for SystemProbe<'_> {
    fn os(&self) -> &str {
        std::env::consts::OS
    }

    fn env(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }

    fn systemd_user_responds(&self) -> bool {
        self.runner
            .run(
                "systemctl",
                &[OsStr::new("--user"), OsStr::new("show-environment")],
            )
            .is_ok_and(|out| out.success())
    }
}

/// The host's service manager, or None (a container, CI, a minimal box).
/// macOS always has launchd for a user session. Linux has systemd `--user`
/// ONLY when a user manager is actually reachable: `$XDG_RUNTIME_DIR` (the
/// user session's runtime dir, where the user bus lives) AND a `systemctl
/// --user` that answers. Both halves, or it is not usable.
pub fn detect(probe: &dyn HostProbe) -> Option<Manager> {
    match probe.os() {
        "macos" => Some(Manager::Launchd),
        "linux" => {
            probe.env("XDG_RUNTIME_DIR")?;
            if !probe.systemd_user_responds() {
                return None;
            }
            Some(Manager::Systemd)
        }
        _ => None,
    }
}

#[cfg(test)]
pub mod fake {
    use super::*;
    use std::collections::HashMap;

    pub struct FakeProbe {
        pub os: String,
        pub env: HashMap<String, String>,
        pub systemd_responds: bool,
    }

    impl FakeProbe {
        pub fn macos() -> FakeProbe {
            FakeProbe {
                os: "macos".to_owned(),
                env: HashMap::from([("HOME".to_owned(), "/Users/x".to_owned())]),
                systemd_responds: false,
            }
        }

        pub fn linux_with_systemd() -> FakeProbe {
            FakeProbe {
                os: "linux".to_owned(),
                env: HashMap::from([
                    ("HOME".to_owned(), "/home/x".to_owned()),
                    ("XDG_RUNTIME_DIR".to_owned(), "/run/user/1000".to_owned()),
                ]),
                systemd_responds: true,
            }
        }

        /// A container / CI box: no user session, no user manager.
        pub fn container() -> FakeProbe {
            FakeProbe {
                os: "linux".to_owned(),
                env: HashMap::from([("HOME".to_owned(), "/root".to_owned())]),
                systemd_responds: false,
            }
        }
    }

    impl HostProbe for FakeProbe {
        fn os(&self) -> &str {
            &self.os
        }

        fn env(&self, key: &str) -> Option<String> {
            self.env.get(key).cloned()
        }

        fn systemd_user_responds(&self) -> bool {
            self.systemd_responds
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::fake::FakeProbe;
    use super::*;

    #[test]
    fn macos_always_has_launchd() {
        assert_eq!(detect(&FakeProbe::macos()), Some(Manager::Launchd));
    }

    #[test]
    fn linux_with_a_live_user_manager_has_systemd() {
        assert_eq!(
            detect(&FakeProbe::linux_with_systemd()),
            Some(Manager::Systemd)
        );
    }

    /// A container/CI box: linux, no user session runtime dir, no user
    /// manager answering. NOT an error — the caller hands off visibly.
    #[test]
    fn a_container_has_no_host_service_manager() {
        assert_eq!(detect(&FakeProbe::container()), None);
    }

    /// Both halves are required: a runtime dir with no answering user
    /// manager is not a usable systemd, and vice versa.
    #[test]
    fn linux_needs_both_a_runtime_dir_and_an_answering_user_manager() {
        let mut no_answer = FakeProbe::linux_with_systemd();
        no_answer.systemd_responds = false;
        assert_eq!(detect(&no_answer), None);

        let mut no_runtime_dir = FakeProbe::linux_with_systemd();
        no_runtime_dir.env.remove("XDG_RUNTIME_DIR");
        assert_eq!(detect(&no_runtime_dir), None);
    }

    #[test]
    fn an_unknown_os_has_no_host_service_manager() {
        let mut other = FakeProbe::macos();
        other.os = "freebsd".to_owned();
        assert_eq!(detect(&other), None);
    }
}

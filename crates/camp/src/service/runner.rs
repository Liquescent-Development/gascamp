//! The process seam. Every `launchctl` / `systemctl` / `id` invocation goes
//! through `CommandRunner`, so the `camp service` FLOWS are testable with no
//! live service manager: production wires `SystemRunner`; tests wire
//! `FakeRunner`, which records the argv it was handed and returns canned
//! outcomes — and ERRORS on an unexpected call, because a fake that guesses
//! hides the bug the test exists to catch.

use std::ffi::OsStr;

use anyhow::{Context, Result, bail};

/// One finished process. `code` is None when a signal killed it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunOutcome {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl RunOutcome {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

pub trait CommandRunner {
    /// Run `program` to completion and capture its outcome. Err ONLY when the
    /// process could not be run at all (not on PATH, no permission). A
    /// non-zero exit is a RESULT, not an error: state queries read it, and
    /// `run_checked` turns it into a loud failure everywhere it must be one.
    fn run(&self, program: &str, args: &[&OsStr]) -> Result<RunOutcome>;
}

pub struct SystemRunner;

impl CommandRunner for SystemRunner {
    fn run(&self, program: &str, args: &[&OsStr]) -> Result<RunOutcome> {
        let output = std::process::Command::new(program)
            .args(args)
            .output()
            .with_context(|| format!("running `{program}`"))?;
        Ok(RunOutcome {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// Run and REQUIRE success. A non-zero exit fails loudly, naming the command
/// and the service manager's own stderr — never swallowed, never retried,
/// never "fall back to something simpler" (invariant 5).
pub fn run_checked(
    runner: &dyn CommandRunner,
    program: &str,
    args: &[&OsStr],
) -> Result<RunOutcome> {
    let outcome = runner.run(program, args)?;
    if !outcome.success() {
        let argv = args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ");
        let code = match outcome.code {
            Some(code) => format!("exit {code}"),
            None => "killed by a signal".to_owned(),
        };
        bail!(
            "`{program} {argv}` failed ({code}): {}",
            outcome.stderr.trim()
        );
    }
    Ok(outcome)
}

/// This process's uid, for launchd's `gui/<uid>` domain target. `id -u` is
/// the portable, dependency-free, `unsafe`-free source (the crate forbids
/// `unsafe`, and libc is not a dependency).
pub fn current_uid(runner: &dyn CommandRunner) -> Result<u32> {
    let out = run_checked(runner, "id", &[OsStr::new("-u")])?;
    out.stdout
        .trim()
        .parse()
        .with_context(|| format!("parsing `id -u` output {:?}", out.stdout))
}

#[cfg(test)]
#[allow(clippy::panic)]
pub mod fake {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    /// Records every argv; returns queued outcomes IN ORDER; errors on a call
    /// it was not told to expect.
    pub struct FakeRunner {
        queued: RefCell<VecDeque<RunOutcome>>,
        calls: RefCell<Vec<Vec<String>>>,
    }

    impl FakeRunner {
        pub fn new(outcomes: Vec<RunOutcome>) -> FakeRunner {
            FakeRunner {
                queued: RefCell::new(outcomes.into()),
                calls: RefCell::new(Vec::new()),
            }
        }

        pub fn ok(stdout: &str) -> RunOutcome {
            RunOutcome {
                code: Some(0),
                stdout: stdout.to_owned(),
                stderr: String::new(),
            }
        }

        pub fn fail(code: i32, stderr: &str) -> RunOutcome {
            RunOutcome {
                code: Some(code),
                stdout: String::new(),
                stderr: stderr.to_owned(),
            }
        }

        /// The argv of call `n`, space-joined — what the test asserts on.
        pub fn call(&self, n: usize) -> String {
            match self.calls.borrow().get(n) {
                Some(argv) => argv.join(" "),
                None => panic!("FakeRunner: no call {n} (there were {})", self.call_count()),
            }
        }

        pub fn call_count(&self) -> usize {
            self.calls.borrow().len()
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, program: &str, args: &[&OsStr]) -> Result<RunOutcome> {
            let mut argv = vec![program.to_owned()];
            argv.extend(args.iter().map(|a| a.to_string_lossy().into_owned()));
            self.calls.borrow_mut().push(argv.clone());
            self.queued
                .borrow_mut()
                .pop_front()
                .with_context(|| format!("FakeRunner: unexpected call `{}`", argv.join(" ")))
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::fake::FakeRunner;
    use super::*;

    /// The real runner runs a real process and captures its outcome.
    #[test]
    fn system_runner_captures_a_real_process() {
        let out = SystemRunner.run("id", &[OsStr::new("-u")]).unwrap();
        assert!(out.success(), "`id -u` must succeed: {out:?}");
        assert!(
            out.stdout.trim().parse::<u32>().is_ok(),
            "stdout was {:?}",
            out.stdout
        );
    }

    /// A non-zero exit is a RESULT for `run` (state queries read it) and a
    /// loud ERROR for `run_checked` (mutating flows must never silence one).
    #[test]
    fn run_checked_fails_loudly_on_a_non_zero_exit() {
        let ok = SystemRunner.run("false", &[]).unwrap();
        assert!(!ok.success(), "`false` exits non-zero");

        let err = run_checked(&SystemRunner, "false", &[]).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("`false"), "must name the command: {msg}");
        assert!(msg.contains("exit 1"), "must name the exit code: {msg}");
    }

    #[test]
    fn current_uid_reads_the_real_uid() {
        let uid = current_uid(&SystemRunner).unwrap();
        let fake = FakeRunner::new(vec![FakeRunner::ok("501\n")]);
        assert_eq!(current_uid(&fake).unwrap(), 501);
        assert_eq!(fake.call(0), "id -u");
        let _ = uid; // the real value varies by host; that it parses is the point
    }

    /// A fake that guesses hides the bug the test exists to catch: an
    /// unexpected call is an error, never a default success.
    #[test]
    fn fake_runner_errors_on_an_unexpected_call() {
        let fake = FakeRunner::new(vec![]);
        let err = fake.run("launchctl", &[OsStr::new("print")]).unwrap_err();
        assert!(format!("{err:#}").contains("unexpected call"), "{err:#}");
    }
}

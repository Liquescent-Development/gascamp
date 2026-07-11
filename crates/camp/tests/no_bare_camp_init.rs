#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! A GATE, not a convention (Phase 2, campd service management).
//!
//! `camp init` now installs a REAL host service unit wherever a service manager
//! exists (design §6). A test that runs a bare `camp init` therefore writes a
//! real LaunchAgent into the developer's — or the macos-latest runner's —
//! ~/Library/LaunchAgents and starts a real campd against a temp directory that
//! is about to be deleted. Every camp-init call in this suite MUST pass
//! --no-service, and this test is what keeps that true as tests are added.
//!
//! An `.arg(…)`/`.args([…])` line mentioning "init" passes only if it carries
//! ONE of three things. The two markers are NOT interchangeable, and a marker
//! that does not describe the line it sits on is a lie that defeats the gate:
//!
//!   `--no-service`     the normal case: a camp init that installs nothing.
//!
//!   `// not-camp:`     it is not the camp binary at all. This suite also runs
//!                      `git init` (many files) and one `bd init`
//!                      (cli_export.rs). Nothing about camp applies to them.
//!
//!   `// real-manager:` a DELIBERATE bare `camp init` — the environment-aware
//!                      default (design §6) — which is legitimate ONLY inside a
//!                      test that is BOTH `#[ignore]`d AND gated on
//!                      CAMP_SERVICE_E2E=1, so `cargo test --workspace` and CI
//!                      never run it and only an operator who typed
//!                      `make service-e2e` can install anything. Today there is
//!                      exactly one: the real-manager lifecycle test in
//!                      cli_service.rs, whose whole purpose is to prove that
//!                      `camp init` DOES install a unit on a host that has a
//!                      service manager. If you reach for this marker anywhere
//!                      else, you are almost certainly writing the bug this
//!                      gate exists to catch: use --no-service instead.
//!
//!                      Because the marker cannot verify its own precondition
//!                      just by sitting on a line, this scan additionally
//!                      requires that the FILE carrying a `real-manager:` line
//!                      contain both `#[ignore` and `CAMP_SERVICE_E2E` — so the
//!                      marker cannot be used to smuggle a bare `camp init`
//!                      into a test that actually runs in CI.
//!
//! The scan is LINE-ORIENTED, not a parser: an init call split across lines
//! (`.arg(\n    "init",\n)`) would slip past it. That is an accepted limit —
//! every call site in this suite is single-line, and the point is to stop the
//! easy, likely regression, not to be a Rust front end.
//!
//! The scan is also NON-RECURSIVE: it reads only the files directly inside
//! `crates/camp/tests/`, not subdirectories (e.g. `tests/fixtures/`). Today
//! nothing under a subdirectory is a `.rs` file that could hide a call site;
//! if that ever changes, this scan must be taught to recurse.

use std::path::Path;

#[test]
fn no_test_invokes_camp_init_without_no_service() {
    let tests = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut violations = Vec::new();

    for entry in std::fs::read_dir(&tests).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        // This file quotes the very patterns it forbids.
        if path.file_name().and_then(|n| n.to_str()) == Some("no_bare_camp_init.rs") {
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap();
        for (i, line) in source.lines().enumerate() {
            let names_init = line.contains("\"init\"");
            let is_arg = line.contains(".arg(") || line.contains(".args(");
            if !(names_init && is_arg) {
                continue;
            }
            let excused = line.contains("--no-service")
                || line.contains("not-camp:")
                || line.contains("real-manager:");
            if !excused {
                violations.push(format!(
                    "{}:{}: {}",
                    path.file_name().unwrap().to_string_lossy(),
                    i + 1,
                    line.trim()
                ));
                continue;
            }
            // The `real-manager:` marker's precondition (an #[ignore]d test
            // gated on CAMP_SERVICE_E2E) cannot be checked from the line
            // alone — check the file it sits in.
            if line.contains("real-manager:")
                && !(source.contains("#[ignore") && source.contains("CAMP_SERVICE_E2E"))
            {
                violations.push(format!(
                    "{}:{}: carries `real-manager:` but this file is not both #[ignore]d \
                     and gated on CAMP_SERVICE_E2E — the marker's precondition does not hold: {}",
                    path.file_name().unwrap().to_string_lossy(),
                    i + 1,
                    line.trim()
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "these lines run a bare `camp init`, which installs a REAL host service unit on any \
         machine that has a service manager (every dev mac; the macos-latest runner). Pass \
         --no-service. The only exemptions are `// not-camp:` (not the camp binary — git/bd) \
         and `// real-manager:` (a deliberate bare init inside an #[ignore]d, \
         CAMP_SERVICE_E2E-gated test); see this test's module docs before using either:\n{}",
        violations.join("\n")
    );
}

# campd Service Management — Phase 2: `camp service` + Unit Generation + Environment-Aware `camp init` — Implementation Plan

> **Plan review: APPROVED 2026-07-10** (independent Opus reviewer, round 3).
> Round 1 REJECT — B1: the `--no-service` sweep table misidentified a `bd init` call in
> `cli_export.rs:221` as `camp init` (executed verbatim it would have corrupted that test, silently,
> because it is `#[ignore]`d and bd-gated), under cover of an unsound verification grep. B2:
> `to_string_lossy()` in both unit generators — a non-UTF-8 camp path would have silently produced a
> well-formed unit naming a directory that does not exist, "successfully installed", then crash-looped
> under KeepAlive/Restart=always while the operator was told it worked (invariant 5 violation).
> Round 2 REJECT — B3: the new `no_bare_camp_init` guard rejected the new lifecycle test's own
> deliberate bare `camp init`, turning `cargo test --workspace` red where the plan promised green.
> B4: the spec amendment targeted a "§5 intro line" that does not exist and missed §4 decision 5,
> the file's only declaration of the verb surface — the spec would have shipped self-contradicting.
> Round 3 APPROVE — the reviewer independently re-derived the sweep (21 files, 33 call sites), ran the
> guard's predicate over every camp-init line left in the tree (including Phase 1's
> `cli_daemon_signal.rs`), opened the spec to verify the amendment anchors, and found no new error.
>
> Non-blocking notes accepted at approval:
> N1 — Task 4 Step 6's intro says "Three edits" but four lettered edits (a)-(d) follow; say "Four".
> N2 — the `real-manager:` marker cannot verify its own precondition; optional in-band hardening is to
> also assert the file contains `#[ignore` and `CAMP_SERVICE_E2E` when a line carries that marker.
> N3 — the sweep table calls Phase 1's init "the one in `campd_stops_gracefully_on_sigterm`"; it
> actually lives in the shared `graceful_stop_on(signal)` helper used by both tests. Prose only —
> one call site, and both the sweep shape and the guard cover it.
> N4 — the guard is line-oriented and non-recursive; both limits are documented in its module docs.
> N5 — `SystemProbe::systemd_user_responds` maps "no systemctl binary" to false. Correct as a
> predicate (it is what makes the container hand-off reachable), not a silenced error.
> N6 — `camp stop` now spawns one `id -u` / `systemctl --user show-environment` per invocation
> (Deviation 4). Not an invariant-1 violation; must be noted in the PR body.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `camp service {install,uninstall,status,restart,list,stop,start}` — the cross-platform control surface that puts a camp's campd under the host's service manager (macOS launchd, Linux systemd `--user`) — plus an environment-aware `camp init` that installs and starts that unit where a manager exists and hands off visibly where none does (containers, CI), plus a `camp stop` that refuses loudly on a supervised camp instead of lying about an effect the supervisor will undo.

**Architecture:** One new module tree, `crates/camp/src/service/`, built around **three ports** so every flow is testable with no live service manager anywhere in unit CI:

1. `CommandRunner` — the ONLY place a real process is spawned (`launchctl`, `systemctl`, `id`). Production wires `SystemRunner`; tests wire `FakeRunner`, which records the argv it was handed and returns canned outcomes.
2. `HostProbe` — the ONLY place the environment is read (OS, env vars, "does `systemctl --user` answer"). Tests wire `FakeProbe`.
3. `Supervisor` — one impl per service manager (`Launchd`, `Systemd`). **Unit-file generation is pure**: `(camp id, camp root, camp binary) → unit text`, and `parse_camp_root` is its exact inverse — the installed unit is the source of truth (design §5: no registry file). The seven `camp service` flows are written ONCE against `&dyn Supervisor`; a third supervisor is a new `impl Supervisor` plus one arm in `supervisor_for`, and nothing else moves.

The `camp service` flows and unit generation are unit-tested against a **real unit directory (a tempdir) with a faked process runner**, so the file IO and the generated text are genuinely exercised while `launchctl`/`systemctl` are not. The end-to-end lifecycle against the host's REAL service manager is an opt-in, local-only, `#[ignore]`d test gated on `CAMP_SERVICE_E2E=1` (mirroring `make e2e` / `make perf`).

**Tech Stack:** Rust (edition 2024); `clap` derive (existing CLI); `anyhow`; `uuid` (already a dep — Phase 2 enables its `v5` feature for a stable, spec'd digest); `tempfile` + `assert_cmd` + `predicates` (existing dev-deps). No new crates.

**Phasing:** This is Phase 2 of the campd-service-management design (`docs/superpowers/specs/2026-07-10-campd-service-management-design.md`). It covers spec §5 (the `camp service` control surface), §6 (environment-aware `camp init`), decisions §4.4 / §4.5, the operator's 2026-07-10 `camp stop` ruling (Task 4 — it needs this phase's supervisor seam, so it is owned here), and the §9 test obligations for unit generation, environment detection, and the opt-in local-only `camp service` integration test.

**Operator decision folded in (2026-07-10):** on a camp with a managed unit, **`camp stop` refuses loudly** rather than issue a socket stop the supervisor would silently undo (`KeepAlive` / `Restart=always`). Its error names the two remedies — so **`camp service stop` and `camp service start` are added** to the §5 surface (additive; nothing is removed), and **this PR amends the feature design spec** (`docs/superpowers/specs/2026-07-10-campd-service-management-design.md` §4 + §5) to record the ruling. Rationale: invariant 5 (fail fast) + invariant 3 (nothing hidden) — no verb may lie about its effect.

---

## Scope boundary — read this before touching anything

Phase 2 **is**: the `camp service` subcommand group, the launchd/systemd unit-file generators, the service-manager detection, `camp init` becoming environment-aware (`--service` / `--no-service`), and — per the operator's 2026-07-10 ruling — `camp stop` refusing on a supervised camp, `camp service stop` / `start`, and the matching amendment to the **feature design spec** (`docs/superpowers/specs/2026-07-10-campd-service-management-design.md`).

Phase 2 is **NOT**, and you must not touch these files or behaviors:

- **SIGTERM/SIGINT handling** (Phase 1, developing in parallel right now in a sibling worktree). Do not edit `crates/camp/src/daemon/mod.rs` or `crates/camp/src/daemon/event_loop.rs`. Phase 2 touches no daemon signal/event-loop code at all.
- **Removing the CLI on-demand auto-start** (`crates/camp/src/daemon/autostart.rs`, `request_with_autostart`) — that is Phase 3. `camp init` may install and start a unit, but the auto-start path stays exactly as it is. Do not delete it, do not rewire it, do not touch `cmd/top.rs`, `cmd/adopt.rs`, or `cmd/sling.rs`.
- **The container reference (`contrib/docker/`) and the v1 design-doc amendments** (`docs/design/2026-07-05-gas-camp-design.md` §5/§9/§12, and folding away the superseded `contrib/launchd/` example) — that is Phase 4. Leave `contrib/launchd/` and `docs/design/2026-07-05-gas-camp-design.md` untouched. (Phase 2 **does** amend the *feature* spec, `docs/superpowers/specs/2026-07-10-campd-service-management-design.md` — that is Task 4, and it is a different file.)

Phase 1 (SIGTERM/SIGINT, PR #69) is merged-pending with CI green; you will rebase onto it. Its file set is `daemon/mod.rs`, `daemon/event_loop.rs`, and a new `crates/camp/tests/cli_daemon_signal.rs`. Phase 2 touches none of the first two. **Keep the `main.rs` footprint to exactly: `mod service;`, `pub mod service;` in the `cmd` block, the `Service` subcommand group, the two `Init` flags, and their dispatch arms.**

`cli_daemon_signal.rs` calls `camp init` with no flags — Task 5's sweep MUST cover it, and Task 5's new `no_bare_camp_init` guard test enforces that automatically rather than relying on anyone remembering.

---

## Global Constraints

Copied from `AGENTS.md` and the design spec — every task implicitly includes these:

- **TDD, strictly.** Write the failing test, RUN it, watch it fail, implement, watch it pass. Never write implementation before its failing test.
- **Fail fast (invariant 5).** No fallbacks, no silenced errors, no placeholders, no "if it doesn't work, try something simpler". A non-zero exit from `launchctl`/`systemctl` in a mutating flow is a loud error carrying the manager's own stderr.
- **No panics in non-test code.** Clippy denies `unwrap_used` / `expect_used` / `panic` workspace-wide; `unsafe_code` is forbidden at the crate root. Test modules opt out with `#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]` (integration tests) or `#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]` on the `mod tests` (unit tests) — copy the existing pattern in `crates/camp/src/daemon/socket.rs`.
- **Nothing hidden (invariant 3).** The unit directory IS the registry — no status file, no registry file, no pidfile. `camp service list` reads live units. The service manager's own words (`state = running`, `LoadState=…`) are printed verbatim.
- **Idle is free (invariant 1).** No ticks, no polling loops in campd or the CLI. (Bounded polling in *test harnesses* is sanctioned and already used — see `crates/camp/tests/e2e.rs`.)
- **No new ledger event types.** Design §10 pins this: `camp service` writes unit files and talks to the OS; it appends nothing to the ledger. The vocabulary mirror (invariant 7) is untouched.
- **Unit CI must stay green on BOTH `ubuntu-latest` and `macos-latest` with no live service manager assumed.** Every unit test uses `FakeRunner`/`FakeProbe` + a tempdir. **No test may invoke `camp init` without `--no-service`** — see the sweep in Task 5; a bare `camp init` on a macOS box installs a REAL LaunchAgent into the developer's (or the runner's) `~/Library/LaunchAgents` and starts a daemon. Task 5 turns that rule into an enforced gate (`tests/no_bare_camp_init.rs`), so it cannot rot.
- **A path that cannot be written into a unit file is a hard error, never a lossy conversion.** Unit text is TEXT (a launchd plist is XML; a systemd unit is line-oriented INI). `to_string_lossy()` on a non-UTF-8 camp path would substitute U+FFFD, produce a well-formed unit naming a directory that does not exist, and let `install` report success while the supervisor respawn-throttles a campd that can never open its camp. Every path that enters a generator passes `service::unit_safe_str` first (Task 2) — valid UTF-8, no control characters, or a loud error. `unit_text` takes `&str`, never `&Path`.
- **With `--no-service`, `camp init` constructs no supervisor and touches no unit directory.** `detect` still runs (it is read-only and cheap), but `decide` returns `SkipByFlag`, so `supervisor_for` is never called and nothing is written. That is why the sweep is sufficient, and why no env switch is needed — a hidden switch would violate invariant 3 (and would silently degrade production `camp init` if it ever leaked).
- Branch: this phase's own branch (`phase-2-camp-service`). Never commit to `main`. No co-author lines, no self-attribution, no AI attribution in any commit or PR body.
- Gates green before every commit: `cargo fmt --all`, then `cargo clippy --workspace --all-targets --all-features -- -D warnings`, then `cargo test --workspace`.
- **Clippy denies unused imports and dead code.** Each task below introduces only items that are *reachable from `main`* by the end of that task — that is why the tasks are vertical slices (list → install/uninstall → status/restart → init) rather than bottom-up layers. When you add code, add exactly the imports that code uses; when a later task adds a method, it adds its imports with it.

---

## File Structure

**New — `crates/camp/src/service/` (the seam):**

| File | Responsibility |
|---|---|
| `service/mod.rs` | Module root: re-exports, `supervisor_for` / `host_supervisor` (wire a `Manager` to this host's unit dir + uid), `unit_safe_str` (the fail-fast path gate, Task 2), and (Task 5) the pure `ServiceChoice` / `Decision` / `decide` for `camp init`. |
| `service/camp_id.rs` | `CampId` — the stable, collision-free, human-readable `<camp-id>` slug. PURE (absolute path → id) plus one thin canonicalizing wrapper. |
| `service/runner.rs` | `CommandRunner` port + `RunOutcome` + `SystemRunner` + `run_checked` + `current_uid`; `FakeRunner` under `#[cfg(test)]`. The only place a process is spawned. |
| `service/supervisor.rs` | The `Supervisor` port (the trait), `UnitState`, `InstalledUnit`, and `scan_units` (the unit dir IS the registry). |
| `service/launchd.rs` | `Launchd` — macOS LaunchAgent: pure plist generation, XML escaping, `launchctl` calls behind the runner. |
| `service/systemd.rs` | `Systemd` — Linux systemd `--user` unit: pure unit generation, `ExecStart` quoting, `systemctl --user` calls behind the runner. |
| `service/detect.rs` | `HostProbe` port + `SystemProbe` + `Manager` + `detect`; `FakeProbe` under `#[cfg(test)]`. |
| `cmd/service.rs` | The seven `camp service` flows, each taking `&dyn Supervisor` (testable), the shared `managed_unit` identity check, plus the thin `run_*` wrappers that build the real host wiring. |

**Modified:**

| File | Change |
|---|---|
| `crates/camp/src/main.rs` | `mod service;`; `pub mod service;` in the `cmd` block; the `Service` subcommand group + `ServiceCommand` enum; `Init`'s `--service` / `--no-service` flags; the dispatch arms. |
| `crates/camp/src/cmd/init.rs` | Takes a `ServiceChoice`; after creating the camp, decides install / skip / hard-fail and prints the hand-off. |
| `crates/camp/src/cmd/stop.rs` | **Refuses loudly on a supervised camp** (operator decision, Task 4): a socket stop the supervisor would undo is a lie about the verb's effect. Unsupervised camps keep today's behavior byte-for-byte. |
| `docs/superpowers/specs/2026-07-10-campd-service-management-design.md` | **Amended in this PR** (Task 4, Step 6), four edits: **§4 gains decision 10** (the `camp stop` ruling); **§4 decision 5** — the spec's ONE declaration of the verb surface, at line 97 — is widened to `{install,uninstall,status,restart,list,stop,start}`; **§5 gains `stop` / `start` bullets**; **§5's `status` bullet** is amended to name `systemctl --user show` (what the code actually runs). AGENTS.md: spec and code never silently diverge. |
| `crates/camp/Cargo.toml` | `uuid` gains the `v5` feature (a stable, spec'd digest for the camp-id hash). |
| `Makefile` | `service-e2e` target (opt-in, local-only), alongside `perf` / `e2e`. |
| `README.md` | A `camp service` subsection under "campd & the daemon model". |
| 22 files in `crates/camp/tests/` | `camp init` → `camp init --no-service` (Task 5 sweep — mandatory; a bare `camp init` in a test installs a real host unit). 33 call sites across 21 files today, plus `cli_daemon_signal.rs` after the Phase 1 rebase. Two **non-camp** init lines (`git`, `bd`) gain a `// not-camp:` marker for the guard test. |

**New tests:**
- `crates/camp/tests/cli_service.rs` — a read-only CLI smoke test that runs in normal CI, plus the `#[ignore]`d, `CAMP_SERVICE_E2E=1`-gated real-manager lifecycle test.
- `crates/camp/tests/no_bare_camp_init.rs` — the **gate** that makes the sweep permanent: it scans the test sources and fails if any camp-init call lacks `--no-service`.

Everything else is unit tests inside the `service/`, `cmd/service.rs` and `cmd/stop.rs` modules (the `camp` crate is a **binary crate with no lib target**, so integration tests cannot reach internals — pure/seam tests MUST be `#[cfg(test)] mod tests` inside `src/`, exactly like `daemon/socket.rs`).

---

## Task 1: The supervisor seam + `camp service list`

The first vertical slice: the three ports, both supervisors' **read** surface, and the `list` verb that proves the unit directory is the registry.

**Files:**
- Create: `crates/camp/src/service/mod.rs`, `service/camp_id.rs`, `service/runner.rs`, `service/supervisor.rs`, `service/launchd.rs`, `service/systemd.rs`, `service/detect.rs`
- Create: `crates/camp/src/cmd/service.rs`
- Create: `crates/camp/tests/cli_service.rs`
- Modify: `crates/camp/src/main.rs` (`mod service;` after `mod gitignore;` on line 5; `pub mod service;` inside the `mod cmd` block between `pub mod search;` (line 23) and `pub mod session;` (line 24); the `Service` variant in `enum Command` after `Backup` (line 275); a new `enum ServiceCommand`; the dispatch arm in `fn run` after the `Command::Backup` arm (line 636))

**Interfaces:**
- Consumes: nothing from other tasks (this is the first).
- Produces, for Tasks 2–6:
  - `service::CampId` — `CampId::from_slug(&str) -> Result<CampId>`, `impl Display`, derives `Clone, Debug, PartialEq, Eq, PartialOrd, Ord`.
  - `service::runner::{CommandRunner, RunOutcome, SystemRunner, run_checked, current_uid}` — `CommandRunner::run(&self, program: &str, args: &[&OsStr]) -> Result<RunOutcome>`; `RunOutcome { code: Option<i32>, stdout: String, stderr: String }` with `success()`; `run_checked(runner: &dyn CommandRunner, program: &str, args: &[&OsStr]) -> Result<RunOutcome>`; `current_uid(runner: &dyn CommandRunner) -> Result<u32>`. `#[cfg(test)] service::runner::fake::FakeRunner`.
  - `service::supervisor::{Supervisor, UnitState, InstalledUnit, scan_units}` — the trait (Task 1 methods: `name`, `unit_path`, `parse_camp_root`, `state`, `installed`; Task 2 adds `unit_text`, `load`, `unload`, `reload_units`; Task 3 adds `restart`).
  - `service::launchd::{Launchd, LABEL_PREFIX}` — `Launchd::new(unit_dir: PathBuf, uid: u32, runner: &dyn CommandRunner)`.
  - `service::systemd::{Systemd, UNIT_PREFIX}` — `Systemd::new(unit_dir: PathBuf, runner: &dyn CommandRunner)`.
  - `service::detect::{HostProbe, SystemProbe, Manager, detect}` — `detect(probe: &dyn HostProbe) -> Option<Manager>`; `#[cfg(test)] service::detect::fake::FakeProbe`.
  - `service::{supervisor_for, host_supervisor}` — `host_supervisor<'a>(probe: &dyn HostProbe, runner: &'a dyn CommandRunner) -> Result<Option<Box<dyn Supervisor + 'a>>>`.
  - `cmd::service::list(supervisor: Option<&dyn Supervisor>) -> Result<String>` and `cmd::service::run_list() -> Result<()>`.

- [ ] **Step 1: Write the failing tests**

Create `crates/camp/src/service/camp_id.rs` with ONLY its test module for now (the impl comes in Step 3 — but the file must contain the module declaration to compile; write the whole file with the tests, then watch it fail to compile, which is the failing state for a fresh module):

Write the tests as the *last* thing in each new file. Concretely, create these seven files, each containing its final `#[cfg(test)] mod tests` block but with the implementation bodies left out — you will get compile errors, which IS the failing state. To keep the TDD loop tight and honest, do it in this order and run the tests after each: (a) `camp_id.rs`, (b) `runner.rs`, (c) `detect.rs`, (d) `supervisor.rs` + `launchd.rs` + `systemd.rs`, (e) `cmd/service.rs`.

The complete test bodies you are driving toward:

`crates/camp/src/service/camp_id.rs` tests:

```rust
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn from_slug_accepts_a_camp_id_and_rejects_anything_else() {
        assert_eq!(CampId::from_slug("dev-f9481b53").unwrap().to_string(), "dev-f9481b53");
        // The id becomes a launchd LABEL and a systemd UNIT NAME. A file we
        // did not write must never become a launchctl argument.
        for bad in ["", "Dev", "dev.1", "dev/1", "../etc", "dev_1", "dev 1"] {
            assert!(CampId::from_slug(bad).is_err(), "{bad:?} must not parse as a camp id");
        }
    }
}
```

`crates/camp/src/service/runner.rs` tests:

```rust
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
        assert!(out.stdout.trim().parse::<u32>().is_ok(), "stdout was {:?}", out.stdout);
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
```

`crates/camp/src/service/detect.rs` tests (design §9: "the detect-service-manager function returns install/skip for representative environments via injected probes"):

```rust
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
        assert_eq!(detect(&FakeProbe::linux_with_systemd()), Some(Manager::Systemd));
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
```

`crates/camp/src/service/launchd.rs` tests (Task 1 half — the read surface):

```rust
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::service::runner::fake::FakeRunner;

    const PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>com.gascamp.campd.dev-f9481b53</string>
  <key>ProgramArguments</key>
  <array>
    <string>/usr/local/bin/camp</string>
    <string>daemon</string>
    <string>--camp</string>
    <string>/Users/x/camps/My Camp &amp; Co/.camp</string>
  </array>
</dict>
</plist>
"#;

    fn id() -> CampId {
        CampId::from_slug("dev-f9481b53").unwrap()
    }

    #[test]
    fn unit_path_is_the_launch_agent_plist() {
        let fake = FakeRunner::new(vec![]);
        let launchd = Launchd::new(PathBuf::from("/Users/x/Library/LaunchAgents"), 501, &fake);
        assert_eq!(
            launchd.unit_path(&id()),
            PathBuf::from("/Users/x/Library/LaunchAgents/com.gascamp.campd.dev-f9481b53.plist")
        );
    }

    /// The unit is the source of truth (design §5: no registry file). The
    /// camp root is read back out of ProgramArguments — the real datum, not
    /// a duplicated marker — and XML-unescaped.
    #[test]
    fn parse_camp_root_reads_the_program_arguments() {
        let fake = FakeRunner::new(vec![]);
        let launchd = Launchd::new(PathBuf::from("/units"), 501, &fake);
        assert_eq!(
            launchd.parse_camp_root(PLIST).unwrap(),
            PathBuf::from("/Users/x/camps/My Camp & Co/.camp")
        );
        assert!(
            launchd.parse_camp_root("<plist></plist>").is_err(),
            "a plist with no --camp is a loud error, never a guess"
        );
    }

    #[test]
    fn state_reads_launchctl_print() {
        let running = FakeRunner::new(vec![FakeRunner::ok(
            "com.gascamp.campd.dev-f9481b53 = {\n\tstate = running\n\tpid = 4242\n}\n",
        )]);
        let launchd = Launchd::new(PathBuf::from("/units"), 501, &running);
        let state = launchd.state(&id()).unwrap();
        assert_eq!(
            state,
            UnitState {
                loaded: true,
                running: true,
                detail: "state = running".to_owned()
            }
        );
        assert_eq!(running.call(0), "launchctl print gui/501/com.gascamp.campd.dev-f9481b53");

        // Booted out: launchctl does not know the label. A STATE, not an error.
        let absent = FakeRunner::new(vec![FakeRunner::fail(113, "Could not find service\n")]);
        let launchd = Launchd::new(PathBuf::from("/units"), 501, &absent);
        let state = launchd.state(&id()).unwrap();
        assert!(!state.loaded && !state.running, "{state:?}");
        assert_eq!(state.detail, "Could not find service");
    }

    /// `list`'s source of truth: the unit DIRECTORY. Files that are not ours
    /// are ignored; a missing directory means zero units, not an error.
    #[test]
    fn installed_enumerates_the_unit_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("com.gascamp.campd.dev-f9481b53.plist"),
            PLIST,
        )
        .unwrap();
        std::fs::write(dir.path().join("com.apple.something.plist"), "<plist/>").unwrap();

        let fake = FakeRunner::new(vec![]);
        let launchd = Launchd::new(dir.path().to_path_buf(), 501, &fake);
        let units = launchd.installed().unwrap();
        assert_eq!(units.len(), 1, "only camp units, and every camp unit: {units:?}");
        assert_eq!(units[0].id, id());
        assert_eq!(units[0].camp_root, PathBuf::from("/Users/x/camps/My Camp & Co/.camp"));
        assert_eq!(
            units[0].unit_path,
            dir.path().join("com.gascamp.campd.dev-f9481b53.plist")
        );

        let missing = Launchd::new(dir.path().join("nope"), 501, &fake);
        assert!(missing.installed().unwrap().is_empty(), "no unit dir = no units");
    }
}
```

`crates/camp/src/service/systemd.rs` tests (Task 1 half):

```rust
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::service::runner::fake::FakeRunner;

    const UNIT: &str = "[Unit]\nDescription=Gas Camp daemon (campd)\n\n[Service]\nType=simple\nExecStart=\"/usr/local/bin/camp\" daemon --camp \"/home/x/my camps/.camp\"\nRestart=always\n";

    fn id() -> CampId {
        CampId::from_slug("dev-f9481b53").unwrap()
    }

    #[test]
    fn unit_path_is_the_user_unit() {
        let fake = FakeRunner::new(vec![]);
        let systemd = Systemd::new(PathBuf::from("/home/x/.config/systemd/user"), &fake);
        assert_eq!(
            systemd.unit_path(&id()),
            PathBuf::from("/home/x/.config/systemd/user/campd-dev-f9481b53.service")
        );
    }

    #[test]
    fn parse_camp_root_reads_exec_start_through_its_quoting() {
        let fake = FakeRunner::new(vec![]);
        let systemd = Systemd::new(PathBuf::from("/units"), &fake);
        assert_eq!(
            systemd.parse_camp_root(UNIT).unwrap(),
            PathBuf::from("/home/x/my camps/.camp")
        );
        assert!(
            systemd.parse_camp_root("[Service]\nExecStart=/bin/true\n").is_err(),
            "a unit with no --camp is a loud error, never a guess"
        );
    }

    #[test]
    fn state_reads_systemctl_show() {
        let fake = FakeRunner::new(vec![FakeRunner::ok(
            "LoadState=loaded\nActiveState=active\nSubState=running\n",
        )]);
        let systemd = Systemd::new(PathBuf::from("/units"), &fake);
        let state = systemd.state(&id()).unwrap();
        assert_eq!(
            state,
            UnitState {
                loaded: true,
                running: true,
                detail: "LoadState=loaded ActiveState=active SubState=running".to_owned()
            }
        );
        assert_eq!(
            fake.call(0),
            "systemctl --user show campd-dev-f9481b53.service \
             --property=LoadState --property=ActiveState --property=SubState"
        );

        let unknown = FakeRunner::new(vec![FakeRunner::ok(
            "LoadState=not-found\nActiveState=inactive\nSubState=dead\n",
        )]);
        let systemd = Systemd::new(PathBuf::from("/units"), &unknown);
        let state = systemd.state(&id()).unwrap();
        assert!(!state.loaded && !state.running, "{state:?}");
    }

    #[test]
    fn installed_enumerates_the_unit_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("campd-dev-f9481b53.service"), UNIT).unwrap();
        std::fs::write(dir.path().join("pipewire.service"), "[Unit]\n").unwrap();

        let fake = FakeRunner::new(vec![]);
        let systemd = Systemd::new(dir.path().to_path_buf(), &fake);
        let units = systemd.installed().unwrap();
        assert_eq!(units.len(), 1, "only camp units: {units:?}");
        assert_eq!(units[0].id, id());
        assert_eq!(units[0].camp_root, PathBuf::from("/home/x/my camps/.camp"));
    }
}
```

`crates/camp/src/cmd/service.rs` tests (Task 1 half):

```rust
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::service::launchd::Launchd;
    use crate::service::runner::fake::FakeRunner;
    use std::path::PathBuf;

    const PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
  <key>ProgramArguments</key>
  <array>
    <string>/usr/local/bin/camp</string>
    <string>daemon</string>
    <string>--camp</string>
    <string>/Users/x/camps/dev/.camp</string>
  </array>
</dict>
</plist>
"#;

    #[test]
    fn list_reports_every_managed_camp_and_its_state() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("com.gascamp.campd.dev-f9481b53.plist"), PLIST).unwrap();
        let fake = FakeRunner::new(vec![FakeRunner::ok(
            "com.gascamp.campd.dev-f9481b53 = {\n\tstate = running\n}\n",
        )]);
        let launchd = Launchd::new(dir.path().to_path_buf(), 501, &fake);

        let report = list(Some(&launchd)).unwrap();
        assert!(report.contains("dev-f9481b53"), "{report}");
        assert!(report.contains("running"), "{report}");
        assert!(report.contains("/Users/x/camps/dev/.camp"), "{report}");
        assert!(report.contains("com.gascamp.campd.dev-f9481b53.plist"), "{report}");
    }

    #[test]
    fn list_with_no_managed_camps_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new(vec![]);
        let launchd = Launchd::new(dir.path().to_path_buf(), 501, &fake);
        assert!(list(Some(&launchd)).unwrap().contains("no camps"), "must state the empty case");
    }

    /// A container/CI box: no host service manager. Reporting that is the
    /// honest answer to the query — not a silent empty list.
    #[test]
    fn list_with_no_host_service_manager_says_so() {
        let report = list(None).unwrap();
        assert!(report.contains("no host service manager"), "{report}");
    }
}
```

And the CLI-level smoke test — create `crates/camp/tests/cli_service.rs`:

```rust
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Phase 2 (campd service management): the `camp service` control surface.
//!
//! The tests in unit CI are READ-ONLY and must pass on a host with a service
//! manager (macOS) and one without (a Linux CI runner): they never install,
//! start or remove a unit. The full lifecycle against the host's REAL service
//! manager is the `#[ignore]`d, CAMP_SERVICE_E2E-gated test added in Task 6.

use assert_cmd::Command;

fn camp() -> Command {
    let mut cmd = Command::cargo_bin("camp").unwrap();
    cmd.env_remove("CAMP_DIR");
    cmd
}

/// `camp service list` is a pure query over the unit directory — the one
/// `camp service` verb that needs no camp at all (design §5: it is the
/// "manage everything" view across every managed camp). It must succeed
/// everywhere, mutate nothing, and print SOMETHING (an answer, never silence).
#[test]
fn service_list_is_a_read_only_query_that_needs_no_camp() {
    let dir = tempfile::tempdir().unwrap();
    let out = camp()
        .current_dir(dir.path())
        .args(["service", "list"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(
        !String::from_utf8_lossy(&out).trim().is_empty(),
        "list must answer the query (managed units, or why there are none)"
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p camp --lib` — Expected: the `camp` crate is a binary, so use `cargo test -p camp --bins` for the in-`src` unit tests and `cargo test -p camp --test cli_service` for the CLI test.

Run: `cargo test -p camp --bins && cargo test -p camp --test cli_service`
Expected: FAIL — compile errors (`cannot find function \`detect\``, `unresolved import \`crate::service\``, …) and, once it compiles enough, the CLI test fails with `error: unrecognized subcommand 'service'`. That compile failure IS the red state for a new module: the tests name types that do not exist.

- [ ] **Step 3: Write `service/camp_id.rs`**

```rust
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
```

(Then the `#[cfg(test)] mod tests` from Step 1.)

- [ ] **Step 4: Write `service/runner.rs`**

```rust
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
        bail!("`{program} {argv}` failed ({code}): {}", outcome.stderr.trim());
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
```

(Then the `#[cfg(test)] mod tests` from Step 1. Note `fake` is `#[cfg(test)]`, so the `panic!` in `FakeRunner::call` needs the module-level allow — put `#[allow(clippy::panic)]` on `pub mod fake` alongside `#[cfg(test)]`.)

- [ ] **Step 5: Write `service/detect.rs`**

```rust
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
            if probe.env("XDG_RUNTIME_DIR").is_none() {
                return None;
            }
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
```

(Then the `#[cfg(test)] mod tests` from Step 1.)

- [ ] **Step 6: Write `service/supervisor.rs`**

```rust
//! The supervisor port. One implementation per service manager. Everything
//! above this trait (the seven `camp service` flows, `camp init`, `camp stop`) is written
//! ONCE and works for every supervisor; adding a third (a container
//! supervisor, a BSD rc) is a new `impl Supervisor` plus one arm in
//! `supervisor_for` — nothing else moves.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::CampId;

/// The service manager's view of one unit. `detail` is the manager's OWN
/// words, printed verbatim (invariant 3: nothing hidden).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnitState {
    pub loaded: bool,
    pub running: bool,
    pub detail: String,
}

/// One installed unit, read back from the unit directory — which IS the
/// registry (design §5: no registry file, no status file).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledUnit {
    pub id: CampId,
    pub camp_root: PathBuf,
    pub unit_path: PathBuf,
}

pub trait Supervisor {
    /// "launchd" | "systemd" — for operator-facing messages.
    fn name(&self) -> &'static str;

    /// PURE: where this camp's unit file lives.
    fn unit_path(&self, id: &CampId) -> PathBuf;

    /// PURE: the camp root recorded in an installed unit's text — the exact
    /// inverse of `unit_text`. The unit is the source of truth.
    fn parse_camp_root(&self, unit_text: &str) -> Result<PathBuf>;

    /// The service manager's load/run state for this unit.
    fn state(&self, id: &CampId) -> Result<UnitState>;

    /// Every camp unit installed for this user, read from the unit directory.
    fn installed(&self) -> Result<Vec<InstalledUnit>>;
}

/// Shared by every supervisor: the unit DIRECTORY is the registry. Returns
/// `(id, unit path, unit text)` for every file named `<prefix><id><suffix>`,
/// sorted by id (stable output). A missing directory means zero units — not
/// an error; any other IO failure is loud.
pub fn scan_units(
    dir: &Path,
    prefix: &str,
    suffix: &str,
) -> Result<Vec<(CampId, PathBuf, String)>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", dir.display())),
    };
    let mut units = Vec::new();
    for entry in entries {
        let entry = entry.with_context(|| format!("reading {}", dir.display()))?;
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        let Some(rest) = name.strip_prefix(prefix) else {
            continue;
        };
        let Some(slug) = rest.strip_suffix(suffix) else {
            continue;
        };
        let id = CampId::from_slug(slug)?;
        let path = entry.path();
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        units.push((id, path, text));
    }
    units.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(units)
}
```

- [ ] **Step 7: Write `service/launchd.rs` (the read surface)**

```rust
//! launchd (macOS): a per-user LaunchAgent in the `gui/<uid>` domain, at
//! `~/Library/LaunchAgents/com.gascamp.campd.<camp-id>.plist`.

use std::ffi::OsStr;
use std::path::PathBuf;

use anyhow::{Context, Result};

use super::CampId;
use super::runner::CommandRunner;
use super::supervisor::{InstalledUnit, Supervisor, UnitState, scan_units};

/// Every camp unit's label starts with this — `camp service list` finds
/// managed camps by it (design §5).
pub const LABEL_PREFIX: &str = "com.gascamp.campd.";
const PLIST_SUFFIX: &str = ".plist";

pub struct Launchd<'a> {
    unit_dir: PathBuf,
    uid: u32,
    runner: &'a dyn CommandRunner,
}

impl<'a> Launchd<'a> {
    pub fn new(unit_dir: PathBuf, uid: u32, runner: &'a dyn CommandRunner) -> Launchd<'a> {
        Launchd {
            unit_dir,
            uid,
            runner,
        }
    }

    fn label(&self, id: &CampId) -> String {
        format!("{LABEL_PREFIX}{id}")
    }

    /// launchd's service target: `gui/<uid>/<label>`.
    fn service_target(&self, id: &CampId) -> String {
        format!("gui/{}/{}", self.uid, self.label(id))
    }
}

impl Supervisor for Launchd<'_> {
    fn name(&self) -> &'static str {
        "launchd"
    }

    fn unit_path(&self, id: &CampId) -> PathBuf {
        self.unit_dir.join(format!("{LABEL_PREFIX}{id}{PLIST_SUFFIX}"))
    }

    fn parse_camp_root(&self, unit_text: &str) -> Result<PathBuf> {
        // ProgramArguments IS the truth: the <string> after the "--camp"
        // <string>. No duplicated marker to drift out of sync.
        let args: Vec<String> = unit_text
            .split("<string>")
            .skip(1)
            .filter_map(|chunk| chunk.split("</string>").next())
            .map(xml_unescape)
            .collect();
        let root = args
            .iter()
            .position(|arg| arg == "--camp")
            .and_then(|i| args.get(i + 1))
            .context("this unit has no `--camp <dir>` in its ProgramArguments")?;
        Ok(PathBuf::from(root))
    }

    fn state(&self, id: &CampId) -> Result<UnitState> {
        let target = self.service_target(id);
        let out = self
            .runner
            .run("launchctl", &[OsStr::new("print"), OsStr::new(&target)])?;
        if !out.success() {
            // launchd does not know this label: the plist may exist while the
            // unit is booted out. A STATE, not an error.
            return Ok(UnitState {
                loaded: false,
                running: false,
                detail: out.stderr.trim().to_owned(),
            });
        }
        let state_line = out
            .stdout
            .lines()
            .map(str::trim)
            .find(|line| line.starts_with("state = "))
            .unwrap_or("state = unknown");
        Ok(UnitState {
            loaded: true,
            running: state_line == "state = running",
            detail: state_line.to_owned(),
        })
    }

    fn installed(&self) -> Result<Vec<InstalledUnit>> {
        scan_units(&self.unit_dir, LABEL_PREFIX, PLIST_SUFFIX)?
            .into_iter()
            .map(|(id, unit_path, text)| {
                let camp_root = self
                    .parse_camp_root(&text)
                    .with_context(|| format!("reading {}", unit_path.display()))?;
                Ok(InstalledUnit {
                    id,
                    camp_root,
                    unit_path,
                })
            })
            .collect()
    }
}

/// A camp path may legally contain `&` or `<`; an escaped plist must survive
/// the round trip back to the real path.
fn xml_unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}
```

(Then the `#[cfg(test)] mod tests` from Step 1.)

- [ ] **Step 8: Write `service/systemd.rs` (the read surface)**

```rust
//! systemd (Linux): a per-user unit in the `--user` manager, at
//! `$XDG_CONFIG_HOME/systemd/user/campd-<camp-id>.service`
//! (default `~/.config/systemd/user`).

use std::ffi::OsStr;
use std::path::PathBuf;

use anyhow::{Context, Result};

use super::CampId;
use super::runner::{CommandRunner, run_checked};
use super::supervisor::{InstalledUnit, Supervisor, UnitState, scan_units};

/// Every camp unit's name starts with this — `camp service list` finds
/// managed camps by it (design §5).
pub const UNIT_PREFIX: &str = "campd-";
const UNIT_SUFFIX: &str = ".service";

pub struct Systemd<'a> {
    unit_dir: PathBuf,
    runner: &'a dyn CommandRunner,
}

impl<'a> Systemd<'a> {
    pub fn new(unit_dir: PathBuf, runner: &'a dyn CommandRunner) -> Systemd<'a> {
        Systemd { unit_dir, runner }
    }

    fn unit_name(&self, id: &CampId) -> String {
        format!("{UNIT_PREFIX}{id}{UNIT_SUFFIX}")
    }
}

impl Supervisor for Systemd<'_> {
    fn name(&self) -> &'static str {
        "systemd"
    }

    fn unit_path(&self, id: &CampId) -> PathBuf {
        self.unit_dir.join(self.unit_name(id))
    }

    fn parse_camp_root(&self, unit_text: &str) -> Result<PathBuf> {
        let exec = unit_text
            .lines()
            .find_map(|line| line.strip_prefix("ExecStart="))
            .context("this unit has no ExecStart= line")?;
        let args = split_exec(exec);
        let root = args
            .iter()
            .position(|arg| arg == "--camp")
            .and_then(|i| args.get(i + 1))
            .context("this unit's ExecStart has no `--camp <dir>`")?;
        Ok(PathBuf::from(root))
    }

    fn state(&self, id: &CampId) -> Result<UnitState> {
        // One machine-readable call. `show` exits 0 even for a unit systemd
        // has never heard of (LoadState=not-found), so this is a state query,
        // not a failure path.
        let name = self.unit_name(id);
        let out = run_checked(
            self.runner,
            "systemctl",
            &[
                OsStr::new("--user"),
                OsStr::new("show"),
                OsStr::new(&name),
                OsStr::new("--property=LoadState"),
                OsStr::new("--property=ActiveState"),
                OsStr::new("--property=SubState"),
            ],
        )?;
        let value = |key: &str| -> String {
            out.stdout
                .lines()
                .find_map(|line| line.strip_prefix(key))
                .unwrap_or("")
                .trim()
                .to_owned()
        };
        let load = value("LoadState=");
        let active = value("ActiveState=");
        let sub = value("SubState=");
        Ok(UnitState {
            loaded: load == "loaded",
            running: active == "active",
            detail: format!("LoadState={load} ActiveState={active} SubState={sub}"),
        })
    }

    fn installed(&self) -> Result<Vec<InstalledUnit>> {
        scan_units(&self.unit_dir, UNIT_PREFIX, UNIT_SUFFIX)?
            .into_iter()
            .map(|(id, unit_path, text)| {
                let camp_root = self
                    .parse_camp_root(&text)
                    .with_context(|| format!("reading {}", unit_path.display()))?;
                Ok(InstalledUnit {
                    id,
                    camp_root,
                    unit_path,
                })
            })
            .collect()
    }
}

/// systemd's `ExecStart` quoting, in reverse: double-quoted arguments (a camp
/// path may contain spaces) with `\"` and `\\` escapes; bare arguments split
/// on whitespace.
fn split_exec(line: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut started = false;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' if quoted => {
                if let Some(escaped) = chars.next() {
                    current.push(escaped);
                }
            }
            '"' => {
                quoted = !quoted;
                started = true;
            }
            ' ' if !quoted => {
                if started {
                    args.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            _ => {
                current.push(c);
                started = true;
            }
        }
    }
    if started {
        args.push(current);
    }
    args
}
```

(Then the `#[cfg(test)] mod tests` from Step 1.)

- [ ] **Step 9: Write `service/mod.rs`**

```rust
//! Host service management (design §5/§6): campd is a supervised foreground
//! process, and the supervisor is environment-provided. This module is the
//! SEAM that makes that pluggable and testable.
//!
//! Three ports, so no flow needs a live service manager to be tested:
//!   - `CommandRunner` (runner.rs) — the only place a process is spawned.
//!   - `HostProbe` (detect.rs) — the only place the environment is read.
//!   - `Supervisor` (supervisor.rs) — one impl per service manager. Unit-file
//!     GENERATION is pure; only load/unload/restart/state touch the manager,
//!     and they do it through the runner.
//!
//! A third supervisor is a new `impl Supervisor` and one arm in
//! `supervisor_for`. Nothing above the trait changes.

pub mod camp_id;
pub mod detect;
pub mod launchd;
pub mod runner;
pub mod supervisor;
pub mod systemd;

use std::path::PathBuf;

use anyhow::{Context, Result};

pub use camp_id::CampId;
pub use detect::{HostProbe, Manager, SystemProbe, detect};
pub use runner::{CommandRunner, SystemRunner};
pub use supervisor::Supervisor;

/// The supervisor for `manager`, wired to THIS host's unit directory (and,
/// for launchd, this user's uid — its domain target needs one).
pub fn supervisor_for<'a>(
    manager: Manager,
    probe: &dyn HostProbe,
    runner: &'a dyn CommandRunner,
) -> Result<Box<dyn Supervisor + 'a>> {
    match manager {
        Manager::Launchd => {
            let unit_dir = home(probe)?.join("Library").join("LaunchAgents");
            let uid = runner::current_uid(runner)?;
            Ok(Box::new(launchd::Launchd::new(unit_dir, uid, runner)))
        }
        Manager::Systemd => {
            let config = match probe.env("XDG_CONFIG_HOME") {
                Some(dir) => PathBuf::from(dir),
                None => home(probe)?.join(".config"),
            };
            let unit_dir = config.join("systemd").join("user");
            Ok(Box::new(systemd::Systemd::new(unit_dir, runner)))
        }
    }
}

/// The host's supervisor, or None when no host service manager is usable (a
/// container, CI, a minimal box). None is a normal answer, not an error — the
/// CALLER decides what it means (`camp init` hands off; `camp service
/// install` fails loudly).
pub fn host_supervisor<'a>(
    probe: &dyn HostProbe,
    runner: &'a dyn CommandRunner,
) -> Result<Option<Box<dyn Supervisor + 'a>>> {
    match detect(probe) {
        Some(manager) => Ok(Some(supervisor_for(manager, probe, runner)?)),
        None => Ok(None),
    }
}

fn home(probe: &dyn HostProbe) -> Result<PathBuf> {
    probe
        .env("HOME")
        .map(PathBuf::from)
        .context("$HOME is not set — cannot locate the user's unit directory")
}
```

- [ ] **Step 10: Write `cmd/service.rs` (the `list` flow)**

```rust
//! `camp service` (design §5): the control surface over the host's service
//! manager. Every flow takes the `Supervisor` PORT, so each is tested against
//! a real unit directory (a tempdir) with a faked process runner — no live
//! service manager anywhere in unit CI.

use anyhow::Result;

use crate::service::{self, Supervisor, SystemProbe, SystemRunner};

/// `camp service list`: every camp with a managed unit, and its state. The
/// unit DIRECTORY is the registry (design §5) — there is no status file, no
/// registry file. Needs no camp: it is the "manage everything" view.
pub fn list(supervisor: Option<&dyn Supervisor>) -> Result<String> {
    let Some(supervisor) = supervisor else {
        return Ok(
            "no host service manager detected (container/CI?) — no managed units\n".to_owned(),
        );
    };
    let units = supervisor.installed()?;
    if units.is_empty() {
        return Ok(format!(
            "no camps have a managed {} unit\n",
            supervisor.name()
        ));
    }
    let mut report = String::new();
    for unit in units {
        let state = supervisor.state(&unit.id)?;
        let mark = match (state.loaded, state.running) {
            (true, true) => "running",
            (true, false) => "loaded",
            (false, _) => "not loaded",
        };
        report.push_str(&format!(
            "{}  {}  {}\n  unit: {}  [{}]\n",
            unit.id,
            mark,
            unit.camp_root.display(),
            unit.unit_path.display(),
            state.detail
        ));
    }
    Ok(report)
}

/// The wiring: the real host, the real process runner.
pub fn run_list() -> Result<()> {
    let runner = SystemRunner;
    let probe = SystemProbe::new(&runner);
    let supervisor = service::host_supervisor(&probe, &runner)?;
    print!("{}", list(supervisor.as_deref())?);
    Ok(())
}
```

(Then the `#[cfg(test)] mod tests` from Step 1.)

- [ ] **Step 11: Wire `main.rs`**

(a) After `mod gitignore;` (line 5), add:

```rust
mod service;
```

(b) Inside the `mod cmd { … }` block, between `pub mod search;` and `pub mod session;`:

```rust
    pub mod service;
```

(c) In `enum Command`, after the `Backup { … }` variant (ends line 275):

```rust
    /// Manage the camp's host service unit (launchd / systemd --user)
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
```

(d) A new subcommand enum, next to `enum OrderCommand` (after line 355):

```rust
#[derive(Subcommand)]
enum ServiceCommand {
    /// Every camp with a managed unit, and its state (needs no camp)
    List,
}
```

(e) In `fn run`, after the `Command::Backup { … }` arm (ends line 636):

```rust
        Command::Service { command } => match command {
            // `list` is the fleet view: it deliberately does NOT resolve a
            // camp — the installed units are the registry (design §5).
            ServiceCommand::List => cmd::service::run_list(),
        },
```

- [ ] **Step 12: Run the tests to verify they pass**

Run: `cargo test -p camp --bins`
Expected: PASS — the `camp_id`, `runner`, `detect`, `launchd`, `systemd`, and `cmd::service` unit tests all green.

Run: `cargo test -p camp --test cli_service`
Expected: PASS — `camp service list` exits 0 and prints an answer on this host (on macOS: your managed units, probably "no camps have a managed launchd unit"; on a Linux CI runner without a user manager: "no host service manager detected").

- [ ] **Step 13: Gates**

Run: `cargo fmt --all`
Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: clean. If clippy reports `dead_code`, you added an item nothing reaches from `main` — it belongs to a later task; remove it now and add it there.

Run: `cargo test --workspace`
Expected: PASS (nothing existing changed behavior).

- [ ] **Step 14: Commit**

```bash
git add crates/camp/src/service crates/camp/src/cmd/service.rs crates/camp/src/main.rs crates/camp/tests/cli_service.rs
git commit -m "feat(service): supervisor seam (runner/probe/supervisor ports) + camp service list"
```

---

## Task 2: `camp service install` + `uninstall` (pure unit generation)

The heart of the phase: the PURE generators (design §5, §9 — "unit-text generators produce the correct launchd plist and systemd unit for a given camp path… no live service manager needed") and the two mutating flows.

**Files:**
- Modify: `crates/camp/Cargo.toml` (uuid `v5` feature)
- Modify: `crates/camp/src/service/camp_id.rs` (add `for_camp`, `from_absolute`, `human_slug`)
- Modify: `crates/camp/src/service/mod.rs` (add `unit_safe_str` — the fail-fast path gate)
- Modify: `crates/camp/src/service/supervisor.rs` (add `unit_name`, `unit_text`, `reload_units`, `load`, `unload` to the trait)
- Modify: `crates/camp/src/service/launchd.rs` (implement them + `xml_escape`)
- Modify: `crates/camp/src/service/systemd.rs` (implement them + `quote`)
- Modify: `crates/camp/src/cmd/service.rs` (add `ManagedUnit`, `managed_unit`, `install`, `uninstall`, `camp_binary`, `require_supervisor`, `run_install`, `run_uninstall`)
- Modify: `crates/camp/src/main.rs` (`ServiceCommand::{Install, Uninstall}` + dispatch)

**Interfaces:**
- Consumes (Task 1): `CampId::from_slug`, `Supervisor`, `Launchd::new`, `Systemd::new`, `run_checked`, `service::host_supervisor`, `FakeRunner`.
- Produces (Tasks 3–6):
  - `CampId::for_camp(root: &Path) -> Result<CampId>` (canonicalizes, then hashes) and `CampId::from_absolute(abs: &Path) -> CampId` (PURE).
  - `service::unit_safe_str<'a>(path: &'a Path, what: &str) -> Result<&'a str>` — the boundary gate: valid UTF-8 and no control characters, or a loud error.
  - `Supervisor::unit_name(&self, id: &CampId) -> String` (PURE — the manager's own name for the unit: a launchd label, a systemd unit name); `Supervisor::unit_text(&self, id: &CampId, camp_root: &str, exe: &str) -> String` (PURE — **`&str`, never `&Path`:** the gate has already run); `Supervisor::reload_units(&self) -> Result<()>`; `Supervisor::load(&self, id: &CampId) -> Result<()>`; `Supervisor::unload(&self, id: &CampId) -> Result<()>`.
  - `cmd::service::ManagedUnit { id: CampId, name: String, path: PathBuf }`, `cmd::service::managed_unit(supervisor: &dyn Supervisor, camp_root: &Path) -> Result<Option<ManagedUnit>>` — **the one place a verb decides "is this camp managed?"**, with the unit's identity verified against the camp root it names — and `cmd::service::require_managed_unit(supervisor: &dyn Supervisor, camp_root: &Path, remedy: &str) -> Result<ManagedUnit>` (the same, with the loud "not managed" error).
  - `cmd::service::install(supervisor: &dyn Supervisor, camp_root: &Path, exe: &Path) -> Result<String>`, `cmd::service::uninstall(supervisor: &dyn Supervisor, camp_root: &Path) -> Result<String>`, `pub(crate) fn camp_binary() -> Result<PathBuf>`, `pub(crate) fn require_supervisor<'a>(probe: &dyn HostProbe, runner: &'a dyn CommandRunner) -> Result<Box<dyn Supervisor + 'a>>`.

- [ ] **Step 1: Write the failing tests**

Add to `crates/camp/src/service/camp_id.rs`'s `mod tests`:

```rust
    use std::path::Path;

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
        let direct = CampId::for_camp(&root).unwrap();
        let indirect = CampId::for_camp(&dir.path().join("sub").join("..").join(".camp")).unwrap();
        assert_eq!(direct, indirect);
        assert!(
            CampId::for_camp(&dir.path().join("nope")).is_err(),
            "a camp that does not exist is a loud error, not a fabricated id"
        );
    }
```

(The `sub` component must exist for `canonicalize` to resolve `sub/..` — create it: add `std::fs::create_dir_all(dir.path().join("sub")).unwrap();` before the `indirect` line.)

Add to `crates/camp/src/service/launchd.rs`'s `mod tests`:

```rust
    /// Design §5: `ProgramArguments = camp daemon --camp <dir>`, `RunAtLoad`
    /// + `KeepAlive`. PURE: a path in, the plist text out. Pinned as a golden
    /// — a supervisor's unit file is an operator-visible artifact. Note the
    /// `&str` parameters: `unit_safe_str` has ALREADY proven the paths are
    /// representable, so no lossy conversion can hide in here.
    #[test]
    fn unit_text_is_the_keepalive_launch_agent() {
        let fake = FakeRunner::new(vec![]);
        let launchd = Launchd::new(PathBuf::from("/units"), 501, &fake);
        let text = launchd.unit_text(&id(), "/Users/x/camps/dev/.camp", "/usr/local/bin/camp");
        assert_eq!(
            text,
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>com.gascamp.campd.dev-f9481b53</string>
  <key>ProgramArguments</key>
  <array>
    <string>/usr/local/bin/camp</string>
    <string>daemon</string>
    <string>--camp</string>
    <string>/Users/x/camps/dev/.camp</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>StandardOutPath</key>
  <string>/dev/null</string>
  <key>StandardErrorPath</key>
  <string>/Users/x/camps/dev/.camp/campd.log</string>
</dict>
</plist>
"#
        );
    }

    /// A camp path may contain XML metacharacters. An unescaped `&` is a
    /// corrupt plist launchd refuses — and generation must survive the round
    /// trip back to the exact path.
    #[test]
    fn unit_text_escapes_xml_and_round_trips() {
        let fake = FakeRunner::new(vec![]);
        let launchd = Launchd::new(PathBuf::from("/units"), 501, &fake);
        let root = "/Users/x/camps/R&D <beta>/.camp";
        let text = launchd.unit_text(&id(), root, "/usr/local/bin/camp");
        assert!(text.contains("R&amp;D &lt;beta&gt;"), "{text}");
        assert!(!text.contains("R&D <beta>"), "raw metacharacters leaked: {text}");
        assert_eq!(launchd.parse_camp_root(&text).unwrap(), PathBuf::from(root));
    }

    #[test]
    fn unit_name_is_the_launchd_label() {
        let fake = FakeRunner::new(vec![]);
        let launchd = Launchd::new(PathBuf::from("/units"), 501, &fake);
        assert_eq!(launchd.unit_name(&id()), "com.gascamp.campd.dev-f9481b53");
    }

    #[test]
    fn load_bootstraps_the_agent_into_the_gui_domain() {
        let fake = FakeRunner::new(vec![FakeRunner::ok("")]);
        let launchd = Launchd::new(PathBuf::from("/units"), 501, &fake);
        launchd.load(&id()).unwrap();
        assert_eq!(
            fake.call(0),
            "launchctl bootstrap gui/501 /units/com.gascamp.campd.dev-f9481b53.plist"
        );
    }

    /// A launchctl failure is LOUD, carrying launchd's own words.
    #[test]
    fn a_failed_bootstrap_is_a_loud_error() {
        let fake = FakeRunner::new(vec![FakeRunner::fail(5, "Input/output error\n")]);
        let launchd = Launchd::new(PathBuf::from("/units"), 501, &fake);
        let err = launchd.load(&id()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("Input/output error"), "must carry launchd's stderr: {msg}");
    }

    /// `bootout` on a unit launchd never bootstrapped fails. We do not guess
    /// and we do not silence: we ASK (`state`) and act on the answer.
    #[test]
    fn unload_boots_out_a_loaded_unit_and_skips_an_unloaded_one() {
        let loaded = FakeRunner::new(vec![
            FakeRunner::ok("com.gascamp.campd.dev-f9481b53 = {\n\tstate = running\n}\n"),
            FakeRunner::ok(""),
        ]);
        let launchd = Launchd::new(PathBuf::from("/units"), 501, &loaded);
        launchd.unload(&id()).unwrap();
        assert_eq!(loaded.call(1), "launchctl bootout gui/501/com.gascamp.campd.dev-f9481b53");

        let absent = FakeRunner::new(vec![FakeRunner::fail(113, "Could not find service\n")]);
        let launchd = Launchd::new(PathBuf::from("/units"), 501, &absent);
        launchd.unload(&id()).unwrap();
        assert_eq!(absent.call_count(), 1, "nothing to boot out: only the state query");
    }
```

(No new imports: `unit_text` takes `&str`, so the launchd test module needs nothing beyond the `PathBuf` it already imports. Clippy denies unused imports — add exactly what each step uses, and no more.)

Add to `crates/camp/src/service/systemd.rs`'s `mod tests`:

```rust
    /// Design §5: `ExecStart=camp daemon --camp <dir>`, `Restart=always`.
    /// PURE, and pinned as a golden.
    #[test]
    fn unit_text_is_the_restart_always_user_unit() {
        let fake = FakeRunner::new(vec![]);
        let systemd = Systemd::new(PathBuf::from("/units"), &fake);
        let text = systemd.unit_text(&id(), "/home/x/camps/dev/.camp", "/usr/local/bin/camp");
        assert_eq!(
            text,
            "[Unit]\n\
             Description=Gas Camp daemon (campd) for /home/x/camps/dev/.camp\n\
             \n\
             [Service]\n\
             Type=simple\n\
             ExecStart=\"/usr/local/bin/camp\" daemon --camp \"/home/x/camps/dev/.camp\"\n\
             Restart=always\n\
             RestartSec=1\n\
             \n\
             [Install]\n\
             WantedBy=default.target\n"
        );
    }

    /// A camp path may contain spaces or a quote; systemd's ExecStart quoting
    /// must survive the round trip back to the exact path.
    #[test]
    fn unit_text_quotes_exec_start_and_round_trips() {
        let fake = FakeRunner::new(vec![]);
        let systemd = Systemd::new(PathBuf::from("/units"), &fake);
        let root = "/home/x/my \"camps\"/.camp";
        let text = systemd.unit_text(&id(), root, "/usr/local/bin/camp");
        assert_eq!(systemd.parse_camp_root(&text).unwrap(), PathBuf::from(root));
    }

    #[test]
    fn unit_name_is_the_systemd_unit_name() {
        let fake = FakeRunner::new(vec![]);
        let systemd = Systemd::new(PathBuf::from("/units"), &fake);
        assert_eq!(systemd.unit_name(&id()), "campd-dev-f9481b53.service");
    }

    #[test]
    fn load_enables_and_starts_the_unit() {
        let fake = FakeRunner::new(vec![FakeRunner::ok("")]);
        let systemd = Systemd::new(PathBuf::from("/units"), &fake);
        systemd.load(&id()).unwrap();
        assert_eq!(
            fake.call(0),
            "systemctl --user enable --now campd-dev-f9481b53.service"
        );
    }

    #[test]
    fn unload_disables_and_stops_the_unit() {
        let fake = FakeRunner::new(vec![FakeRunner::ok("")]);
        let systemd = Systemd::new(PathBuf::from("/units"), &fake);
        systemd.unload(&id()).unwrap();
        assert_eq!(
            fake.call(0),
            "systemctl --user disable --now campd-dev-f9481b53.service"
        );
    }

    #[test]
    fn reload_units_tells_systemd_the_unit_dir_changed() {
        let fake = FakeRunner::new(vec![FakeRunner::ok("")]);
        let systemd = Systemd::new(PathBuf::from("/units"), &fake);
        systemd.reload_units().unwrap();
        assert_eq!(fake.call(0), "systemctl --user daemon-reload");
    }
```

(No new imports: `unit_text` takes `&str`, so the systemd test module needs no `Path` import beyond the `PathBuf` it already uses.)

And the **B2 gate** — the pure boundary check. Add a `#[cfg(test)] mod tests` to `crates/camp/src/service/mod.rs` (it grows a second test in Task 5):

```rust
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::path::Path;

    /// A unit file is TEXT — a launchd plist is XML, a systemd unit is
    /// line-oriented INI. A path that cannot be written into one is a HARD
    /// ERROR at the boundary, never a lossy conversion: `to_string_lossy`
    /// would substitute U+FFFD, produce a well-formed unit naming a directory
    /// that does not exist, and let `install` report success while the
    /// supervisor respawn-throttles a campd that can never open its camp.
    #[test]
    fn a_path_that_cannot_be_written_into_a_unit_is_a_loud_error() {
        assert_eq!(
            unit_safe_str(Path::new("/Users/x/camps/dev/.camp"), "camp").unwrap(),
            "/Users/x/camps/dev/.camp"
        );

        // Not valid UTF-8 (legal on macOS and Linux alike).
        use std::os::unix::ffi::OsStrExt as _;
        let raw = std::ffi::OsStr::from_bytes(b"/tmp/caf\xFF/.camp");
        let err = unit_safe_str(Path::new(raw), "camp").unwrap_err();
        assert!(format!("{err:#}").contains("not valid UTF-8"), "{err:#}");

        // A control character would structurally corrupt either unit format.
        let err = unit_safe_str(Path::new("/tmp/two\nlines/.camp"), "camp").unwrap_err();
        assert!(format!("{err:#}").contains("control character"), "{err:#}");
    }
}
```

Add to `crates/camp/src/cmd/service.rs`'s `mod tests`:

```rust
    /// The full install flow against a REAL unit directory (a tempdir) with a
    /// faked service manager: the unit lands on disk with the camp's real
    /// (canonicalized) path, and the manager is asked to load it.
    #[test]
    fn install_writes_the_unit_then_loads_it() {
        let camp = tempfile::tempdir().unwrap();
        let units = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new(vec![FakeRunner::ok("")]); // bootstrap
        let launchd = Launchd::new(units.path().join("LaunchAgents"), 501, &fake);

        let report = install(&launchd, camp.path(), Path::new("/usr/local/bin/camp")).unwrap();

        let id = crate::service::CampId::for_camp(camp.path()).unwrap();
        let unit_path = launchd.unit_path(&id);
        assert!(unit_path.exists(), "the unit must be on disk: {}", unit_path.display());
        let text = std::fs::read_to_string(&unit_path).unwrap();
        let canonical = std::fs::canonicalize(camp.path()).unwrap();
        assert_eq!(launchd.parse_camp_root(&text).unwrap(), canonical);
        assert!(text.contains("<key>KeepAlive</key>"), "{text}");
        assert!(fake.call(0).starts_with("launchctl bootstrap gui/501 "), "{}", fake.call(0));
        assert!(report.contains("installed"), "{report}");
    }

    /// Never a silent overwrite: an existing unit is a hard error naming the
    /// two verbs that CAN act on it.
    #[test]
    fn install_refuses_to_clobber_an_existing_unit() {
        let camp = tempfile::tempdir().unwrap();
        let units = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new(vec![FakeRunner::ok("")]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &fake);
        install(&launchd, camp.path(), Path::new("/usr/local/bin/camp")).unwrap();

        let fake2 = FakeRunner::new(vec![]);
        let launchd2 = Launchd::new(units.path().to_path_buf(), 501, &fake2);
        let err = install(&launchd2, camp.path(), Path::new("/usr/local/bin/camp")).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("already installed"), "{msg}");
        assert!(msg.contains("camp service restart"), "must name the remedy: {msg}");
        assert_eq!(fake2.call_count(), 0, "a refused install touches nothing");
    }

    /// Fail fast, no half state: a unit the manager REFUSES to load must not be
    /// left on disk pretending to be installed.
    #[test]
    fn a_failed_load_rolls_the_unit_file_back() {
        let camp = tempfile::tempdir().unwrap();
        let units = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new(vec![FakeRunner::fail(5, "Bootstrap failed: 5\n")]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &fake);

        let err = install(&launchd, camp.path(), Path::new("/usr/local/bin/camp")).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("Bootstrap failed"), "must carry the manager's words: {msg}");

        let id = crate::service::CampId::for_camp(camp.path()).unwrap();
        assert!(
            !launchd.unit_path(&id).exists(),
            "a unit that would not load must not survive the failed install"
        );
    }

    #[test]
    fn uninstall_unloads_then_removes_the_unit() {
        let camp = tempfile::tempdir().unwrap();
        let units = tempfile::tempdir().unwrap();
        let install_runner = FakeRunner::new(vec![FakeRunner::ok("")]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &install_runner);
        install(&launchd, camp.path(), Path::new("/usr/local/bin/camp")).unwrap();
        let id = crate::service::CampId::for_camp(camp.path()).unwrap();
        let unit_path = launchd.unit_path(&id);

        let uninstall_runner = FakeRunner::new(vec![
            FakeRunner::ok("service = {\n\tstate = running\n}\n"), // state: loaded
            FakeRunner::ok(""),                                    // bootout
        ]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &uninstall_runner);
        let report = uninstall(&launchd, camp.path()).unwrap();

        assert!(uninstall_runner.call(1).starts_with("launchctl bootout "), "{}", uninstall_runner.call(1));
        assert!(!unit_path.exists(), "the unit file must be gone");
        assert!(report.contains("uninstalled"), "{report}");
    }

    /// Uninstalling what is not installed is an error, not a no-op (fail fast).
    #[test]
    fn uninstall_without_a_unit_is_a_loud_error() {
        let camp = tempfile::tempdir().unwrap();
        let units = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new(vec![]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &fake);
        let err = uninstall(&launchd, camp.path()).unwrap_err();
        assert!(format!("{err:#}").contains("no launchd unit"), "{err:#}");
        assert_eq!(fake.call_count(), 0);
    }

    /// B2, the launchd half: a camp path that cannot be written into a unit is
    /// refused BEFORE anything is generated, loaded, or reported as installed.
    /// (A newline is valid UTF-8 and a legal directory name on both macOS and
    /// Linux, so this is creatable everywhere; the non-UTF-8 half of the gate
    /// is pinned purely in `service::tests` — APFS refuses to create such a
    /// directory, so it cannot be exercised through the filesystem on macOS.)
    #[test]
    fn install_refuses_a_camp_path_no_unit_could_name_launchd() {
        let parent = tempfile::tempdir().unwrap();
        let camp = parent.path().join("two\nlines");
        std::fs::create_dir(&camp).unwrap();
        let units = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new(vec![]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &fake);

        let err = install(&launchd, &camp, Path::new("/usr/local/bin/camp")).unwrap_err();
        assert!(format!("{err:#}").contains("control character"), "{err:#}");
        assert_eq!(fake.call_count(), 0, "nothing may be loaded");
        assert!(
            std::fs::read_dir(units.path()).unwrap().next().is_none(),
            "no unit file may be written"
        );
    }

    /// B2, the systemd half: same gate, same refusal.
    #[test]
    fn install_refuses_a_camp_path_no_unit_could_name_systemd() {
        let parent = tempfile::tempdir().unwrap();
        let camp = parent.path().join("two\nlines");
        std::fs::create_dir(&camp).unwrap();
        let units = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new(vec![]);
        let systemd = Systemd::new(units.path().to_path_buf(), &fake);

        let err = install(&systemd, &camp, Path::new("/usr/local/bin/camp")).unwrap_err();
        assert!(format!("{err:#}").contains("control character"), "{err:#}");
        assert_eq!(fake.call_count(), 0, "nothing may be loaded");
        assert!(
            std::fs::read_dir(units.path()).unwrap().next().is_none(),
            "no unit file may be written"
        );
    }

    /// Note 3: the rollback tells the MANAGER too — systemd keeps a failed
    /// unit in memory until the next daemon-reload. (launchd's `reload_units`
    /// is a documented no-op: it reads the plist at bootstrap.)
    #[test]
    fn a_failed_load_rolls_back_the_file_and_the_manager() {
        let camp = tempfile::tempdir().unwrap();
        let units = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new(vec![
            FakeRunner::ok(""),                              // daemon-reload (after write)
            FakeRunner::fail(1, "Failed to enable unit\n"),  // enable --now
            FakeRunner::ok(""),                              // daemon-reload (after rollback)
        ]);
        let systemd = Systemd::new(units.path().to_path_buf(), &fake);

        let err = install(&systemd, camp.path(), Path::new("/usr/local/bin/camp")).unwrap_err();
        assert!(format!("{err:#}").contains("Failed to enable unit"), "{err:#}");
        assert_eq!(fake.call(0), "systemctl --user daemon-reload");
        assert_eq!(fake.call(2), "systemctl --user daemon-reload");
        assert!(
            std::fs::read_dir(units.path()).unwrap().next().is_none(),
            "the unit file must not survive a failed load"
        );
    }

    /// Note 2: `<camp-id>` is `<slug>-<32 bits>`, so a collision — however
    /// unlikely — must never let one camp's verb act on ANOTHER camp's unit.
    /// The unit is the source of truth, so we ASK it which camp it names.
    /// (The collision is simulated by rewriting the installed unit's camp
    /// path: an id collision is exactly "the unit at my path names someone
    /// else's camp", and that is the state the guard must catch.)
    #[test]
    fn a_unit_that_names_another_camp_is_never_acted_on() {
        let camp = tempfile::tempdir().unwrap();
        let units = tempfile::tempdir().unwrap();
        let install_runner = FakeRunner::new(vec![FakeRunner::ok("")]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &install_runner);
        install(&launchd, camp.path(), Path::new("/usr/local/bin/camp")).unwrap();

        let id = crate::service::CampId::for_camp(camp.path()).unwrap();
        let unit_path = launchd.unit_path(&id);
        let text = std::fs::read_to_string(&unit_path).unwrap();
        let hijacked = text.replace(
            &std::fs::canonicalize(camp.path()).unwrap().display().to_string(),
            "/Users/someone/else/.camp",
        );
        std::fs::write(&unit_path, hijacked).unwrap();

        let fake = FakeRunner::new(vec![]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &fake);
        let err = uninstall(&launchd, camp.path()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("/Users/someone/else/.camp"), "must name the other camp: {msg}");
        assert_eq!(fake.call_count(), 0, "another camp's daemon is never touched");
        assert!(unit_path.exists(), "and another camp's unit is never removed");
    }
```

(Add `use std::path::Path;` and `use crate::service::systemd::Systemd;` to that test module's imports.)

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p camp --bins`
Expected: FAIL — `no method named \`unit_text\` found`, `no function or associated item named \`for_camp\``, `cannot find function \`install\``, etc.

- [ ] **Step 3: Enable uuid's `v5` feature**

In `crates/camp/Cargo.toml`, change the `uuid` line to:

```toml
uuid = { version = "1.23.4", features = ["v4", "v5"] }
```

- [ ] **Step 4: Add the id derivation to `service/camp_id.rs`**

Add `use std::path::Path;` and `anyhow::Context` to the imports, and these to `impl CampId`:

```rust
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
        let digest = uuid::Uuid::new_v5(
            &uuid::Uuid::NAMESPACE_OID,
            absolute.as_os_str().as_bytes(),
        );
        let hash: String = digest.simple().to_string().chars().take(8).collect();
        CampId(format!("{}-{hash}", human_slug(absolute)))
    }
```

And the pure slug helper, at file scope:

```rust
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
```

- [ ] **Step 4b: Add the path gate to `service/mod.rs`**

```rust
/// The boundary gate for everything that enters a unit file.
///
/// A unit is TEXT: a launchd plist is XML, a systemd unit is line-oriented
/// INI. A path that is not valid UTF-8 (legal on macOS and Linux), or that
/// carries a control character, cannot be written into either without
/// corrupting it — and a corrupt unit the manager still ACCEPTS is the worst
/// outcome available: `install` prints "now supervised", and the supervisor
/// respawn-throttles a campd that can never open its camp. `to_string_lossy`
/// would do exactly that (U+FFFD for the unrepresentable bytes), which is the
/// silent-fallback pattern invariant 5 exists to forbid. So we refuse HERE,
/// loudly, before a single byte of unit text is generated — and `unit_text`
/// takes `&str`, so no generator can reintroduce the lossy path.
pub fn unit_safe_str<'a>(path: &'a Path, what: &str) -> Result<&'a str> {
    let text = path.to_str().with_context(|| {
        format!(
            "the {what} path is not valid UTF-8 ({}) — no service unit can name it; \
             move the camp to a UTF-8 path, or run `camp daemon --camp <dir>` under \
             your own supervisor",
            path.display()
        )
    })?;
    if let Some(bad) = text.chars().find(|c| c.is_control()) {
        bail!(
            "the {what} path contains a control character ({bad:?}) — no service unit can \
             name it (a launchd plist is XML; a systemd unit is line-oriented): {}",
            path.display()
        );
    }
    Ok(text)
}
```

(`service/mod.rs` gains `use std::path::Path;` and `anyhow::bail` for this. `path.display()` inside the error is the standard lossy *rendering* of a path for human eyes — it decorates an error that is already loud; nothing is silenced.)

- [ ] **Step 5: Grow the `Supervisor` trait (`service/supervisor.rs`)**

Add to the trait, after `unit_path`:

```rust
    /// PURE: the service manager's OWN name for this unit — a launchd label
    /// (`com.gascamp.campd.<id>`), a systemd unit name (`campd-<id>.service`).
    /// Operator-facing: every message about a unit names it.
    fn unit_name(&self, id: &CampId) -> String;

    /// PURE: the unit's text. `(camp id, camp root, camp binary) → plist /
    /// unit file`. No IO, no environment — this is the function design §9
    /// requires to be unit-tested without a live service manager.
    ///
    /// The paths arrive as `&str`, NOT `&Path`: `service::unit_safe_str` has
    /// already proven they are representable in a unit file. A generator that
    /// took `&Path` would need a lossy conversion, and a lossy conversion here
    /// produces a "successfully installed" unit pointing at a directory that
    /// does not exist (invariant 5).
    fn unit_text(&self, id: &CampId, camp_root: &str, exe: &str) -> String;
```

and after `parse_camp_root`:

```rust
    /// Tell the service manager the unit DIRECTORY changed. Called after a
    /// unit file is written and after one is removed. launchd reads the plist
    /// at bootstrap, so it is a no-op there; systemd needs `daemon-reload`.
    fn reload_units(&self) -> Result<()>;

    /// Load + start an already-written unit.
    fn load(&self, id: &CampId) -> Result<()>;

    /// Stop + unload a unit. Its file is removed by the caller (the unit
    /// directory is the registry, and the flow that owns it does the IO).
    fn unload(&self, id: &CampId) -> Result<()>;
```

- [ ] **Step 6: Implement them for launchd (`service/launchd.rs`)**

Add `use super::runner::run_checked;` to the imports, add `fn domain` to the inherent impl:

```rust
    /// launchd's per-user domain target.
    fn domain(&self) -> String {
        format!("gui/{}", self.uid)
    }
```

and add to `impl Supervisor for Launchd<'_>`:

```rust
    fn unit_name(&self, id: &CampId) -> String {
        self.label(id)
    }

    fn unit_text(&self, id: &CampId, camp_root: &str, exe: &str) -> String {
        // KeepAlive (design §4.2, always-on): the supervisor keeps campd
        // alive; a crash is restarted. StandardErrorPath is the camp's own
        // campd.log (CampDir::log_path) — a supervised daemon's stderr is
        // never swallowed (invariant 3). No lossy conversion anywhere: the
        // caller passed strings `unit_safe_str` already vouched for.
        let log = format!("{camp_root}/campd.log");
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{exe}</string>
    <string>daemon</string>
    <string>--camp</string>
    <string>{root}</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>StandardOutPath</key>
  <string>/dev/null</string>
  <key>StandardErrorPath</key>
  <string>{log}</string>
</dict>
</plist>
"#,
            label = xml_escape(&self.label(id)),
            exe = xml_escape(exe),
            root = xml_escape(camp_root),
            log = xml_escape(&log),
        )
    }

    fn reload_units(&self) -> Result<()> {
        // launchd reads the plist at bootstrap time: there is nothing to
        // reload. Stated, not silently skipped.
        Ok(())
    }

    fn load(&self, id: &CampId) -> Result<()> {
        let unit_path = self.unit_path(id);
        run_checked(
            self.runner,
            "launchctl",
            &[
                OsStr::new("bootstrap"),
                OsStr::new(&self.domain()),
                unit_path.as_os_str(),
            ],
        )?;
        Ok(())
    }

    fn unload(&self, id: &CampId) -> Result<()> {
        // `bootout` on a label launchd never bootstrapped fails. We do not
        // guess and we do not silence a failure: we ASK for the state and act
        // on the answer. A bootout of a LOADED unit that fails is still loud.
        if !self.state(id)?.loaded {
            return Ok(());
        }
        run_checked(
            self.runner,
            "launchctl",
            &[
                OsStr::new("bootout"),
                OsStr::new(&self.service_target(id)),
            ],
        )?;
        Ok(())
    }
```

and the escaping half of the round trip, at file scope next to `xml_unescape`:

```rust
/// XML text escaping for the plist. A camp path may legally contain `&` or
/// `<`; an unescaped one is a corrupt plist launchd refuses to load. `&` FIRST
/// (it is the escape introducer), the inverse of `xml_unescape`.
fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
```

- [ ] **Step 7: Implement them for systemd (`service/systemd.rs`)**

Add to `impl Supervisor for Systemd<'_>`:

```rust
    fn unit_name(&self, id: &CampId) -> String {
        format!("{UNIT_PREFIX}{id}{UNIT_SUFFIX}")
    }

    fn unit_text(&self, _id: &CampId, camp_root: &str, exe: &str) -> String {
        // Restart=always (design §4.2, always-on). Output goes to the journal
        // (`journalctl --user -u campd-<id>`): visible, not swallowed. The
        // paths are `&str` that `unit_safe_str` vouched for — control-character
        // free, so neither the unquoted Description= nor the line-oriented
        // parse can be structurally corrupted by a path.
        format!(
            "[Unit]\n\
             Description=Gas Camp daemon (campd) for {camp_root}\n\
             \n\
             [Service]\n\
             Type=simple\n\
             ExecStart={exe} daemon --camp {camp}\n\
             Restart=always\n\
             RestartSec=1\n\
             \n\
             [Install]\n\
             WantedBy=default.target\n",
            exe = quote(exe),
            camp = quote(camp_root),
        )
    }

    fn reload_units(&self) -> Result<()> {
        run_checked(
            self.runner,
            "systemctl",
            &[OsStr::new("--user"), OsStr::new("daemon-reload")],
        )?;
        Ok(())
    }

    fn load(&self, id: &CampId) -> Result<()> {
        let name = self.unit_name(id);
        run_checked(
            self.runner,
            "systemctl",
            &[
                OsStr::new("--user"),
                OsStr::new("enable"),
                OsStr::new("--now"),
                OsStr::new(&name),
            ],
        )?;
        Ok(())
    }

    fn unload(&self, id: &CampId) -> Result<()> {
        let name = self.unit_name(id);
        run_checked(
            self.runner,
            "systemctl",
            &[
                OsStr::new("--user"),
                OsStr::new("disable"),
                OsStr::new("--now"),
                OsStr::new(&name),
            ],
        )?;
        Ok(())
    }
```

**Delete the private inherent `fn unit_name` that Task 1 put on `Systemd`** — the trait method above replaces it verbatim (an inherent method would shadow the trait method and duplicate the format string). Every `self.unit_name(id)` inside `impl Supervisor for Systemd` then resolves to the trait method, unchanged. (`Launchd` keeps its private `label()`: `service_target()` uses it, and the trait's `unit_name` delegates to it.)

And, at file scope next to `split_exec`:

```rust
/// systemd's `ExecStart` quoting: every argument double-quoted, with `\` and
/// `"` escaped — a camp path with a space must reach campd verbatim. The
/// inverse of `split_exec`.
fn quote(arg: &str) -> String {
    let escaped = arg.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}
```

Note: `Description=` embeds the raw path (a comment field, not an argument) — that is why the golden pins it unquoted. It is safe to do so precisely because `unit_safe_str` has already excluded newlines.

- [ ] **Step 8: Add the `install` / `uninstall` flows (`cmd/service.rs`)**

Extend the imports to exactly: `use std::path::{Path, PathBuf};`, `use anyhow::{Context, Result, bail};`, `use crate::campdir::CampDir;`, `use crate::service::{self, CampId, Supervisor, SystemProbe, SystemRunner};`.

**Do NOT add `CommandRunner` / `HostProbe` to that list.** `require_supervisor`'s signature below names them through the `service::` path (`&dyn service::HostProbe`, `&'a dyn service::CommandRunner`) and nothing in this file uses them bare — importing them would be an unused import, and `-D warnings` rejects that. Then add:

```rust
/// The unit installed for THIS camp — identity verified.
pub(crate) struct ManagedUnit {
    pub id: CampId,
    /// The manager's own name for it (a launchd label; a systemd unit name).
    pub name: String,
    pub path: PathBuf,
}

/// Is this camp managed, and is the unit at its path really ITS unit?
///
/// The one place any verb answers "is this camp supervised?" — `install`'s
/// clobber check, `uninstall`, `status`, `restart`, `stop`, `start`, and
/// `camp stop`'s refusal all go through here.
///
/// `<camp-id>` is `<slug>-<32 bits of digest>`: collision is vanishingly
/// unlikely, but "the file exists" alone would let a colliding camp operate on
/// ANOTHER camp's unit — and `uninstall` would remove it. So we do not trust
/// the path; we ASK the unit which camp it names (the unit is the source of
/// truth, design §5) and refuse loudly on a mismatch.
pub(crate) fn managed_unit(
    supervisor: &dyn Supervisor,
    camp_root: &Path,
) -> Result<Option<ManagedUnit>> {
    let id = CampId::for_camp(camp_root)?;
    let path = supervisor.unit_path(&id);
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("reading {}", path.display()))?;
    let named = supervisor.parse_camp_root(&text)?;
    let canonical = std::fs::canonicalize(camp_root)
        .with_context(|| format!("resolving the camp path {}", camp_root.display()))?;
    if named != canonical {
        bail!(
            "the {} unit {} names a DIFFERENT camp ({}) than this one ({}) — the camp id \
             {} collides. Refusing to act on another camp's daemon; move or rename this camp.",
            supervisor.name(),
            path.display(),
            named.display(),
            canonical.display(),
            id
        );
    }
    Ok(Some(ManagedUnit {
        name: supervisor.unit_name(&id),
        id,
        path,
    }))
}

/// `camp service install` (design §5): generate the unit, then load it.
/// macOS → a KeepAlive LaunchAgent bootstrapped into `gui/$UID`; Linux → a
/// `Restart=always` systemd user unit, `enable --now`.
pub fn install(supervisor: &dyn Supervisor, camp_root: &Path, exe: &Path) -> Result<String> {
    // Never a silent overwrite — and if the unit at our path belongs to a
    // different camp, `managed_unit` refuses rather than let us clobber it.
    if let Some(existing) = managed_unit(supervisor, camp_root)? {
        bail!(
            "a {} unit for this camp is already installed ({} at {}) — \
             `camp service restart` cycles it, `camp service uninstall` removes it",
            supervisor.name(),
            existing.name,
            existing.path.display()
        );
    }
    let id = CampId::for_camp(camp_root)?;
    // The unit must name the camp's REAL path: a supervisor runs campd from
    // its own cwd, and a relative path would resolve somewhere else entirely.
    let root = std::fs::canonicalize(camp_root)
        .with_context(|| format!("resolving the camp path {}", camp_root.display()))?;
    // The gate (invariant 5): a path no unit file could name is a hard error
    // HERE — before any text is generated, any file is written, and any
    // manager is told a camp is supervised.
    let root_text = service::unit_safe_str(&root, "camp")?;
    let exe_text = service::unit_safe_str(exe, "camp binary")?;

    let unit_path = supervisor.unit_path(&id);
    if let Some(parent) = unit_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(&unit_path, supervisor.unit_text(&id, root_text, exe_text))
        .with_context(|| format!("writing {}", unit_path.display()))?;
    supervisor.reload_units()?;

    if let Err(load_error) = supervisor.load(&id) {
        // Fail fast, no half state: a unit the manager refused must not be
        // left on disk pretending to be installed — and the MANAGER must be
        // told too (systemd keeps a failed unit in memory until the next
        // daemon-reload). Every error is reported; none is swallowed.
        let error = load_error.context(format!(
            "loading the {} unit {} ({})",
            supervisor.name(),
            supervisor.unit_name(&id),
            unit_path.display()
        ));
        return Err(match std::fs::remove_file(&unit_path) {
            Err(e) => error.context(format!(
                "and the unit file could not be rolled back: removing {} ({e})",
                unit_path.display()
            )),
            Ok(()) => match supervisor.reload_units() {
                Err(e) => error.context(format!(
                    "and the manager could not be reloaded after the rollback: {e:#}"
                )),
                Ok(()) => error,
            },
        });
    }
    Ok(format!(
        "installed {} unit {} ({})\ncampd for {} is now supervised — it restarts on crash \
         and at login\nto stop it: `camp service stop`; to un-manage it: \
         `camp service uninstall`; to cycle it after an upgrade: `camp service restart`\n",
        supervisor.name(),
        supervisor.unit_name(&id),
        unit_path.display(),
        root.display()
    ))
}

/// The managed unit, or the loud "this camp is not managed" error. `remedy` is
/// the verb that WOULD help — every one of these errors is actionable.
/// (Shared by `uninstall`, `restart`, `stop` and `start`: four verbs, one
/// sentence about what "not installed" means.)
pub(crate) fn require_managed_unit(
    supervisor: &dyn Supervisor,
    camp_root: &Path,
    remedy: &str,
) -> Result<ManagedUnit> {
    match managed_unit(supervisor, camp_root)? {
        Some(unit) => Ok(unit),
        None => {
            let id = CampId::for_camp(camp_root)?;
            bail!(
                "no {} unit is installed for this camp ({} does not exist) — {remedy}",
                supervisor.name(),
                supervisor.unit_path(&id).display()
            )
        }
    }
}

/// `camp service uninstall` (design §5): stop + unload + remove the unit.
pub fn uninstall(supervisor: &dyn Supervisor, camp_root: &Path) -> Result<String> {
    let unit = require_managed_unit(supervisor, camp_root, "nothing to uninstall")?;
    supervisor.unload(&unit.id)?;
    std::fs::remove_file(&unit.path)
        .with_context(|| format!("removing {}", unit.path.display()))?;
    supervisor.reload_units()?;
    Ok(format!(
        "uninstalled {} unit {} ({})\n",
        supervisor.name(),
        unit.name,
        unit.path.display()
    ))
}

/// The `camp` binary a unit must run: the running executable's REAL absolute
/// path. A unit naming a relative path breaks the moment the supervisor's cwd
/// differs from yours (it always does).
pub(crate) fn camp_binary() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("locating the camp binary")?;
    std::fs::canonicalize(&exe).with_context(|| format!("resolving {}", exe.display()))
}

/// The host's supervisor, or the loud, actionable error for a host that has
/// none (a container, CI) — where installing a unit is impossible, not
/// merely inconvenient.
fn require_supervisor<'a>(
    probe: &dyn service::HostProbe,
    runner: &'a dyn service::CommandRunner,
) -> Result<Box<dyn Supervisor + 'a>> {
    service::host_supervisor(probe, runner)?.context(
        "no host service manager detected (macOS launchd, or a reachable systemd --user) — \
         run `camp daemon --camp <dir>` under your supervisor (e.g. the container runtime)",
    )
}

pub fn run_install(camp: &CampDir) -> Result<()> {
    let runner = SystemRunner;
    let probe = SystemProbe::new(&runner);
    let supervisor = require_supervisor(&probe, &runner)?;
    print!(
        "{}",
        install(supervisor.as_ref(), &camp.root, &camp_binary()?)?
    );
    Ok(())
}

pub fn run_uninstall(camp: &CampDir) -> Result<()> {
    let runner = SystemRunner;
    let probe = SystemProbe::new(&runner);
    let supervisor = require_supervisor(&probe, &runner)?;
    print!("{}", uninstall(supervisor.as_ref(), &camp.root)?);
    Ok(())
}
```

(No import changes for `require_supervisor`: it names `HostProbe` / `CommandRunner` through the `service::` path, deliberately — see the import note at the top of this step.)

- [ ] **Step 9: Wire `main.rs`**

Extend `enum ServiceCommand`:

```rust
#[derive(Subcommand)]
enum ServiceCommand {
    /// Install and start this camp's host service unit
    Install,
    /// Stop, unload and remove this camp's host service unit
    Uninstall,
    /// Every camp with a managed unit, and its state (needs no camp)
    List,
}
```

and the dispatch arm:

```rust
        Command::Service { command } => match command {
            ServiceCommand::Install => {
                let camp = CampDir::resolve(cli.camp.as_deref())?;
                cmd::service::run_install(&camp)
            }
            ServiceCommand::Uninstall => {
                let camp = CampDir::resolve(cli.camp.as_deref())?;
                cmd::service::run_uninstall(&camp)
            }
            // `list` is the fleet view: it deliberately does NOT resolve a
            // camp — the installed units are the registry (design §5).
            ServiceCommand::List => cmd::service::run_list(),
        },
```

- [ ] **Step 10: Run the tests to verify they pass**

Run: `cargo test -p camp --bins`
Expected: PASS — including the two golden unit-text tests and both round-trip tests.

- [ ] **Step 11: Gates**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace`
Expected: all clean/PASS.

- [ ] **Step 12: Commit**

```bash
git add crates/camp/Cargo.toml Cargo.lock crates/camp/src/service crates/camp/src/cmd/service.rs crates/camp/src/main.rs
git commit -m "feat(service): pure launchd/systemd unit generation + camp service install/uninstall"
```

---

## Task 3: `camp service status` + `restart` + `stop` + `start`

`stop` / `start` are the operator's 2026-07-10 ruling made real: `camp stop` (Task 4) refuses on a supervised camp and names `camp service stop` as the remedy — so the remedy has to exist first.

**Files:**
- Modify: `crates/camp/src/service/supervisor.rs` (add `restart`, `stop`, `start` to the trait)
- Modify: `crates/camp/src/service/launchd.rs`, `crates/camp/src/service/systemd.rs` (implement them)
- Modify: `crates/camp/src/cmd/service.rs` (add `status`, `restart`, `stop`, `start` + their `run_*` wrappers)
- Modify: `crates/camp/src/main.rs` (`ServiceCommand::{Status, Restart, Stop, Start}` + dispatch)

**Interfaces:**
- Consumes (Tasks 1–2): `Supervisor`, `CampId::for_camp`, `cmd::service::{managed_unit, require_managed_unit, require_supervisor}`, `crate::campdir::CampDir`, and the existing daemon socket client `crate::daemon::socket::{self, Request, Response}` (`socket::request_if_up(camp, &Request::Status) -> Result<Option<Response>>` — campd-not-listening is `Ok(None)`; a campd that accepts and does not answer is the loud `CampdUnresponsive` error).
- Produces (Tasks 4–6): `Supervisor::restart(&self, id: &CampId) -> Result<()>`, `Supervisor::stop(&self, id: &CampId) -> Result<()>`, `Supervisor::start(&self, id: &CampId) -> Result<()>`; `cmd::service::status(supervisor: Option<&dyn Supervisor>, camp: &CampDir) -> Result<String>`; `cmd::service::{restart, stop, start}(supervisor: &dyn Supervisor, camp_root: &Path) -> Result<String>`.

- [ ] **Step 1: Write the failing tests**

Add to `crates/camp/src/service/launchd.rs`'s `mod tests`:

```rust
    /// Design §5: restart = `launchctl kickstart -k` (the post-upgrade cycle).
    #[test]
    fn restart_kickstarts_the_service() {
        let fake = FakeRunner::new(vec![FakeRunner::ok("")]);
        let launchd = Launchd::new(PathBuf::from("/units"), 501, &fake);
        launchd.restart(&id()).unwrap();
        assert_eq!(
            fake.call(0),
            "launchctl kickstart -k gui/501/com.gascamp.campd.dev-f9481b53"
        );
    }

    /// The operator's remedy (2026-07-10). launchd has no "stop but stay
    /// bootstrapped" for a KeepAlive agent — `launchctl kill` would just be
    /// restarted — so stopping IS booting out of the domain, and starting IS
    /// bootstrapping back in. The plist stays on disk (that is what makes this
    /// `stop`, not `uninstall`).
    #[test]
    fn stop_boots_the_agent_out_and_start_bootstraps_it_back() {
        let stopping = FakeRunner::new(vec![
            FakeRunner::ok("service = {\n\tstate = running\n}\n"), // state: loaded
            FakeRunner::ok(""),                                    // bootout
        ]);
        let launchd = Launchd::new(PathBuf::from("/units"), 501, &stopping);
        launchd.stop(&id()).unwrap();
        assert_eq!(
            stopping.call(1),
            "launchctl bootout gui/501/com.gascamp.campd.dev-f9481b53"
        );

        let starting = FakeRunner::new(vec![FakeRunner::ok("")]);
        let launchd = Launchd::new(PathBuf::from("/units"), 501, &starting);
        launchd.start(&id()).unwrap();
        assert_eq!(
            starting.call(0),
            "launchctl bootstrap gui/501 /units/com.gascamp.campd.dev-f9481b53.plist"
        );
    }
```

Add to `crates/camp/src/service/systemd.rs`'s `mod tests`:

```rust
    /// Design §5: restart = `systemctl --user restart`.
    #[test]
    fn restart_restarts_the_unit() {
        let fake = FakeRunner::new(vec![FakeRunner::ok("")]);
        let systemd = Systemd::new(PathBuf::from("/units"), &fake);
        systemd.restart(&id()).unwrap();
        assert_eq!(fake.call(0), "systemctl --user restart campd-dev-f9481b53.service");
    }

    /// The operator's remedy (2026-07-10). Unlike launchd, systemd separates
    /// "stop the service" from "unload the unit": `stop` leaves it enabled
    /// (so it returns at login), `disable --now` (that is `unload`) does not.
    #[test]
    fn stop_and_start_are_the_unit_level_verbs() {
        let stopping = FakeRunner::new(vec![FakeRunner::ok("")]);
        let systemd = Systemd::new(PathBuf::from("/units"), &stopping);
        systemd.stop(&id()).unwrap();
        assert_eq!(stopping.call(0), "systemctl --user stop campd-dev-f9481b53.service");

        let starting = FakeRunner::new(vec![FakeRunner::ok("")]);
        let systemd = Systemd::new(PathBuf::from("/units"), &starting);
        systemd.start(&id()).unwrap();
        assert_eq!(starting.call(0), "systemctl --user start campd-dev-f9481b53.service");
    }
```

Add to `crates/camp/src/cmd/service.rs`'s `mod tests`:

```rust
    /// Design §5: status is TWO independent truths — the unit's load/run state
    /// AND the campd liveness answer. A loaded unit whose campd does not
    /// answer is exactly the fault this command exists to show.
    #[test]
    fn status_reports_the_unit_and_the_campd_liveness_answer() {
        let camp_dir = tempfile::tempdir().unwrap();
        let units = tempfile::tempdir().unwrap();
        let camp = crate::campdir::CampDir {
            root: camp_dir.path().to_path_buf(),
        };
        let install_runner = FakeRunner::new(vec![FakeRunner::ok("")]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &install_runner);
        install(&launchd, &camp.root, Path::new("/usr/local/bin/camp")).unwrap();

        let status_runner = FakeRunner::new(vec![FakeRunner::ok(
            "service = {\n\tstate = running\n}\n",
        )]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &status_runner);
        let report = status(Some(&launchd), &camp).unwrap();

        assert!(report.contains("running"), "the unit's state: {report}");
        // No campd is listening on this temp camp's socket — and that is a
        // REPORTED state, not an error, and never an auto-start.
        assert!(report.contains("campd: not listening"), "{report}");
    }

    #[test]
    fn status_without_a_unit_says_so_and_names_the_remedy() {
        let camp_dir = tempfile::tempdir().unwrap();
        let units = tempfile::tempdir().unwrap();
        let camp = crate::campdir::CampDir {
            root: camp_dir.path().to_path_buf(),
        };
        let fake = FakeRunner::new(vec![]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &fake);
        let report = status(Some(&launchd), &camp).unwrap();
        assert!(report.contains("not installed"), "{report}");
        assert!(report.contains("camp service install"), "must name the remedy: {report}");
        assert_eq!(fake.call_count(), 0, "no unit file, nothing to ask the manager");
    }

    /// In a container there is no unit — but campd's liveness is still the
    /// half of the answer that matters there.
    #[test]
    fn status_with_no_host_service_manager_still_answers_for_campd() {
        let camp_dir = tempfile::tempdir().unwrap();
        let camp = crate::campdir::CampDir {
            root: camp_dir.path().to_path_buf(),
        };
        let report = status(None, &camp).unwrap();
        assert!(report.contains("no host service manager"), "{report}");
        assert!(report.contains("campd: not listening"), "{report}");
    }

    #[test]
    fn restart_cycles_an_installed_unit_and_refuses_a_missing_one() {
        let camp_dir = tempfile::tempdir().unwrap();
        let units = tempfile::tempdir().unwrap();
        let missing = FakeRunner::new(vec![]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &missing);
        let err = restart(&launchd, camp_dir.path()).unwrap_err();
        assert!(format!("{err:#}").contains("camp service install"), "{err:#}");
        assert_eq!(missing.call_count(), 0);

        let install_runner = FakeRunner::new(vec![FakeRunner::ok("")]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &install_runner);
        install(&launchd, camp_dir.path(), Path::new("/usr/local/bin/camp")).unwrap();

        let restart_runner = FakeRunner::new(vec![FakeRunner::ok("")]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &restart_runner);
        let report = restart(&launchd, camp_dir.path()).unwrap();
        assert!(restart_runner.call(0).starts_with("launchctl kickstart -k "), "{}", restart_runner.call(0));
        assert!(report.contains("restarted"), "{report}");
    }

    /// `camp service stop` / `start` (operator decision, 2026-07-10): the
    /// supervisor-level verbs that `camp stop` points a supervised operator at.
    /// The unit STAYS installed — that is the whole difference from uninstall.
    #[test]
    fn stop_and_start_act_on_the_installed_unit_and_leave_it_installed() {
        let camp_dir = tempfile::tempdir().unwrap();
        let units = tempfile::tempdir().unwrap();
        let install_runner = FakeRunner::new(vec![FakeRunner::ok("")]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &install_runner);
        install(&launchd, camp_dir.path(), Path::new("/usr/local/bin/camp")).unwrap();
        let id = crate::service::CampId::for_camp(camp_dir.path()).unwrap();
        let unit_path = launchd.unit_path(&id);

        let stop_runner = FakeRunner::new(vec![
            FakeRunner::ok("service = {\n\tstate = running\n}\n"),
            FakeRunner::ok(""),
        ]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &stop_runner);
        let report = stop(&launchd, camp_dir.path()).unwrap();
        assert!(report.contains("stopped"), "{report}");
        assert!(unit_path.exists(), "stop must NOT remove the unit (that is uninstall)");

        let start_runner = FakeRunner::new(vec![FakeRunner::ok("")]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &start_runner);
        let report = start(&launchd, camp_dir.path()).unwrap();
        assert!(start_runner.call(0).starts_with("launchctl bootstrap "), "{}", start_runner.call(0));
        assert!(report.contains("started"), "{report}");
    }

    /// Stopping/starting what was never installed is an error, not a no-op.
    #[test]
    fn stop_and_start_without_a_unit_are_loud_errors() {
        let camp_dir = tempfile::tempdir().unwrap();
        let units = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new(vec![]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &fake);
        assert!(format!("{:#}", stop(&launchd, camp_dir.path()).unwrap_err()).contains("no launchd unit"));
        assert!(format!("{:#}", start(&launchd, camp_dir.path()).unwrap_err()).contains("no launchd unit"));
        assert_eq!(fake.call_count(), 0);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p camp --bins`
Expected: FAIL — `no method named \`restart\` found for …`, `cannot find function \`status\` in this scope`.

- [ ] **Step 3: Add `restart` / `stop` / `start` to the trait and both impls**

`service/supervisor.rs`, in the trait after `unload`:

```rust
    /// Cycle the service (the post-upgrade path: a running campd keeps
    /// executing the OLD binary until it is restarted — design §1).
    fn restart(&self, id: &CampId) -> Result<()>;

    /// Stop the service, leaving the unit INSTALLED (operator decision,
    /// 2026-07-10: this is what `camp stop` points a supervised operator at —
    /// a socket stop would just be undone by the supervisor).
    fn stop(&self, id: &CampId) -> Result<()>;

    /// Start a stopped, still-installed unit.
    fn start(&self, id: &CampId) -> Result<()>;
```

`service/launchd.rs`, in `impl Supervisor`:

```rust
    fn restart(&self, id: &CampId) -> Result<()> {
        run_checked(
            self.runner,
            "launchctl",
            &[
                OsStr::new("kickstart"),
                OsStr::new("-k"),
                OsStr::new(&self.service_target(id)),
            ],
        )?;
        Ok(())
    }

    fn stop(&self, id: &CampId) -> Result<()> {
        // launchd has no "stop but stay bootstrapped" for a KeepAlive agent:
        // `launchctl kill` sends a signal and KeepAlive restarts it. Stopping
        // IS booting out of the gui domain — the plist stays on disk, which is
        // exactly what separates this from `uninstall`. Same operation as
        // `unload`, stated rather than aliased so the intent is readable.
        self.unload(id)
    }

    fn start(&self, id: &CampId) -> Result<()> {
        // …and starting is bootstrapping the still-present plist back in.
        self.load(id)
    }
```

`service/systemd.rs`, in `impl Supervisor`:

```rust
    fn restart(&self, id: &CampId) -> Result<()> {
        let name = self.unit_name(id);
        run_checked(
            self.runner,
            "systemctl",
            &[
                OsStr::new("--user"),
                OsStr::new("restart"),
                OsStr::new(&name),
            ],
        )?;
        Ok(())
    }

    fn stop(&self, id: &CampId) -> Result<()> {
        // Unlike launchd, systemd separates the service from the unit: `stop`
        // leaves it ENABLED (it returns at the next login), `disable --now`
        // (our `unload`) does not.
        let name = self.unit_name(id);
        run_checked(
            self.runner,
            "systemctl",
            &[OsStr::new("--user"), OsStr::new("stop"), OsStr::new(&name)],
        )?;
        Ok(())
    }

    fn start(&self, id: &CampId) -> Result<()> {
        let name = self.unit_name(id);
        run_checked(
            self.runner,
            "systemctl",
            &[OsStr::new("--user"), OsStr::new("start"), OsStr::new(&name)],
        )?;
        Ok(())
    }
```

- [ ] **Step 4: Add the `status` / `restart` flows (`cmd/service.rs`)**

Add `use crate::daemon::socket::{self, Request, Response};` to the imports, then:

```rust
/// `camp service status` (design §5): the unit's load/run state, PLUS the
/// campd liveness answer. Two independent truths — a loaded unit whose campd
/// does not answer is precisely the fault worth seeing.
pub fn status(supervisor: Option<&dyn Supervisor>, camp: &CampDir) -> Result<String> {
    let mut report = String::new();
    match supervisor {
        None => report.push_str("unit:  no host service manager detected (container/CI?)\n"),
        // `managed_unit` — not a bare `unit_path.exists()` — so a unit that
        // names a different camp is reported as the loud collision it is,
        // rather than as this camp's state.
        Some(supervisor) => match managed_unit(supervisor, &camp.root)? {
            Some(unit) => {
                let state = supervisor.state(&unit.id)?;
                report.push_str(&format!(
                    "unit:  {} ({}, {})\n       loaded={} running={}  [{}]\n",
                    unit.name,
                    supervisor.name(),
                    unit.path.display(),
                    state.loaded,
                    state.running,
                    state.detail
                ));
            }
            None => {
                let id = CampId::for_camp(&camp.root)?;
                report.push_str(&format!(
                    "unit:  not installed ({} does not exist) — `camp service install`\n",
                    supervisor.unit_path(&id).display()
                ));
            }
        },
    }
    // Liveness is an ANSWERED REQUEST (spec §5 as amended by issue #55), never
    // a bare connect: a wedged campd's listen backlog accepts connections its
    // event loop never serves. This never auto-starts; a campd that accepts
    // and does not answer surfaces as the loud CampdUnresponsive error.
    match socket::request_if_up(camp, &Request::Status)? {
        Some(Response::Status {
            summary,
            red,
            campd_pid,
            ..
        }) => report.push_str(&format!(
            "campd: listening (pid {campd_pid}) — {} live sessions, {} ready, {} red\n",
            summary.live_sessions.len(),
            summary.ready,
            red
        )),
        Some(other) => bail!("unexpected response to status: {other:?}"),
        None => report.push_str(&format!(
            "campd: not listening ({})\n",
            camp.socket_path().display()
        )),
    }
    Ok(report)
}

/// `camp service restart` (design §5): cycle the daemon — the post-upgrade
/// path (`launchctl kickstart -k` / `systemctl --user restart`).
pub fn restart(supervisor: &dyn Supervisor, camp_root: &Path) -> Result<String> {
    let unit = require_managed_unit(supervisor, camp_root, "`camp service install` first")?;
    supervisor.restart(&unit.id)?;
    Ok(format!(
        "restarted {} unit {} ({})\n",
        supervisor.name(),
        unit.name,
        unit.path.display()
    ))
}

/// `camp service stop` (operator decision, 2026-07-10): stop the supervised
/// campd — the verb `camp stop` sends a supervised operator to. The unit stays
/// INSTALLED; `camp service start` brings it back, `camp service uninstall`
/// removes it for good.
pub fn stop(supervisor: &dyn Supervisor, camp_root: &Path) -> Result<String> {
    let unit = require_managed_unit(supervisor, camp_root, "nothing to stop")?;
    supervisor.stop(&unit.id)?;
    Ok(format!(
        "stopped {} unit {} ({})\nthe unit is still installed — `camp service start` \
         brings campd back; `camp service uninstall` removes it\n",
        supervisor.name(),
        unit.name,
        unit.path.display()
    ))
}

/// `camp service start` (operator decision, 2026-07-10): start a stopped but
/// still-installed unit.
pub fn start(supervisor: &dyn Supervisor, camp_root: &Path) -> Result<String> {
    let unit = require_managed_unit(supervisor, camp_root, "`camp service install` first")?;
    supervisor.start(&unit.id)?;
    Ok(format!(
        "started {} unit {} ({})\n",
        supervisor.name(),
        unit.name,
        unit.path.display()
    ))
}

pub fn run_status(camp: &CampDir) -> Result<()> {
    let runner = SystemRunner;
    let probe = SystemProbe::new(&runner);
    // No supervisor is a normal state for `status` (a container still has a
    // campd to report on) — it is only fatal for the MUTATING verbs.
    let supervisor = service::host_supervisor(&probe, &runner)?;
    print!("{}", status(supervisor.as_deref(), camp)?);
    Ok(())
}

pub fn run_restart(camp: &CampDir) -> Result<()> {
    let runner = SystemRunner;
    let probe = SystemProbe::new(&runner);
    let supervisor = require_supervisor(&probe, &runner)?;
    print!("{}", restart(supervisor.as_ref(), &camp.root)?);
    Ok(())
}

pub fn run_stop(camp: &CampDir) -> Result<()> {
    let runner = SystemRunner;
    let probe = SystemProbe::new(&runner);
    let supervisor = require_supervisor(&probe, &runner)?;
    print!("{}", stop(supervisor.as_ref(), &camp.root)?);
    Ok(())
}

pub fn run_start(camp: &CampDir) -> Result<()> {
    let runner = SystemRunner;
    let probe = SystemProbe::new(&runner);
    let supervisor = require_supervisor(&probe, &runner)?;
    print!("{}", start(supervisor.as_ref(), &camp.root)?);
    Ok(())
}
```

- [ ] **Step 5: Wire `main.rs`**

```rust
#[derive(Subcommand)]
enum ServiceCommand {
    /// Install and start this camp's host service unit
    Install,
    /// Stop, unload and remove this camp's host service unit
    Uninstall,
    /// The unit's state and campd's liveness
    Status,
    /// Cycle the daemon (the post-upgrade path)
    Restart,
    /// Stop the supervised campd (the unit stays installed)
    Stop,
    /// Start a stopped but still-installed unit
    Start,
    /// Every camp with a managed unit, and its state (needs no camp)
    List,
}
```

and, in the `Command::Service` dispatch, add before the `List` arm:

```rust
            ServiceCommand::Status => {
                let camp = CampDir::resolve(cli.camp.as_deref())?;
                cmd::service::run_status(&camp)
            }
            ServiceCommand::Restart => {
                let camp = CampDir::resolve(cli.camp.as_deref())?;
                cmd::service::run_restart(&camp)
            }
            ServiceCommand::Stop => {
                let camp = CampDir::resolve(cli.camp.as_deref())?;
                cmd::service::run_stop(&camp)
            }
            ServiceCommand::Start => {
                let camp = CampDir::resolve(cli.camp.as_deref())?;
                cmd::service::run_start(&camp)
            }
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p camp --bins`
Expected: PASS.

- [ ] **Step 7: Gates**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace`
Expected: all clean/PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/camp/src/service crates/camp/src/cmd/service.rs crates/camp/src/main.rs
git commit -m "feat(service): camp service status/restart/stop/start"
```

---

## Task 4: `camp stop` refuses on a supervised camp + the spec amendment

The operator's 2026-07-10 ruling (`docs/superpowers/specs/2026-07-10-campd-service-management-design.md`, amended in this task). A supervised campd is kept alive by `KeepAlive` / `Restart=always`: a socket `Request::Stop` makes campd exit and the supervisor brings it straight back. `camp stop` printing *"campd stopped"* about a daemon that is already returning is a verb lying about its effect — invariant 5 (fail fast) and invariant 3 (nothing hidden) both forbid it. So it refuses, and names the remedies Task 3 just built.

**Files:**
- Modify: `crates/camp/src/service/supervisor.rs` (add `restart_policy` to the trait)
- Modify: `crates/camp/src/service/launchd.rs`, `crates/camp/src/service/systemd.rs` (implement it)
- Modify: `crates/camp/src/cmd/stop.rs` (the supervised-camp check + its unit tests)
- Modify: `docs/superpowers/specs/2026-07-10-campd-service-management-design.md` (§4 decision record + §5 command list + the §5 `status` bullet)

**Interfaces:**
- Consumes (Tasks 1–3): `service::{host_supervisor, SystemProbe, SystemRunner, Supervisor}`, `cmd::service::managed_unit`, and the existing `crate::daemon::socket::{self, Request, Response}` + `CampdUnresponsive` behavior in `cmd/stop.rs` (unchanged for unsupervised camps).
- Produces: `Supervisor::restart_policy(&self) -> &'static str` (`"KeepAlive"` / `"Restart=always"` — the always-on mechanism that makes a socket stop a lie); `cmd::stop::run` unchanged in signature.

- [ ] **Step 1: Write the failing tests**

Add to `crates/camp/src/cmd/stop.rs` (a new test module):

```rust
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::service::launchd::Launchd;
    use crate::service::runner::fake::FakeRunner;
    use std::path::Path;

    /// Operator decision (2026-07-10): on a SUPERVISED camp, `camp stop`
    /// refuses. A socket stop would succeed and the supervisor would restart
    /// campd within moments — so "campd stopped" would be a lie, and no verb
    /// may lie about its effect (invariants 3 and 5). The error names the
    /// supervisor, the unit, the always-on mechanism, and BOTH remedies.
    #[test]
    fn stop_refuses_on_a_supervised_camp_and_sends_no_socket_request() {
        let camp_dir = tempfile::tempdir().unwrap();
        let units = tempfile::tempdir().unwrap();
        let camp = CampDir {
            root: camp_dir.path().to_path_buf(),
        };
        let install_runner = FakeRunner::new(vec![FakeRunner::ok("")]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &install_runner);
        crate::cmd::service::install(&launchd, &camp.root, Path::new("/usr/local/bin/camp"))
            .unwrap();

        let err = run_with(&camp, Some(&launchd)).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("supervised by launchd"), "{msg}");
        assert!(msg.contains("com.gascamp.campd."), "must name the unit: {msg}");
        assert!(msg.contains("KeepAlive"), "must name the always-on mechanism: {msg}");
        assert!(msg.contains("camp service stop"), "must name the remedy: {msg}");
        assert!(msg.contains("camp service uninstall"), "must name the un-manage remedy: {msg}");
        // And it must not have been a socket error dressed up: there is no
        // campd on this temp camp's socket at all — the refusal came FIRST.
        assert!(!msg.contains("not running"), "the refusal precedes any socket attempt: {msg}");
    }

    /// An UNSUPERVISED camp (a container, CI, a camp nobody installed a unit
    /// for) keeps today's behavior exactly: the socket stop is attempted, and
    /// with no campd listening it is the same loud "campd is not running".
    #[test]
    fn stop_on_an_unsupervised_camp_is_unchanged() {
        let camp_dir = tempfile::tempdir().unwrap();
        let units = tempfile::tempdir().unwrap();
        let camp = CampDir {
            root: camp_dir.path().to_path_buf(),
        };
        let fake = FakeRunner::new(vec![]);
        let launchd = Launchd::new(units.path().to_path_buf(), 501, &fake);

        // No unit installed → the supervised check passes through…
        let err = run_with(&camp, Some(&launchd)).unwrap_err();
        assert!(
            format!("{err:#}").contains("campd is not running"),
            "the socket stop must still be attempted: {err:#}"
        );
        assert_eq!(fake.call_count(), 0, "no unit file, nothing to ask the manager");

        // …and so does a host with no service manager at all (a container).
        let err = run_with(&camp, None).unwrap_err();
        assert!(format!("{err:#}").contains("campd is not running"), "{err:#}");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p camp --bins -- cmd::stop`
Expected: FAIL — `cannot find function \`run_with\` in this scope`.

- [ ] **Step 3: Add `restart_policy` to the trait and both impls**

`service/supervisor.rs`, in the trait:

```rust
    /// The always-on mechanism this supervisor uses to keep campd alive — the
    /// reason a socket-level `camp stop` would be undone. Operator-facing:
    /// `camp stop`'s refusal names it, so the operator can see WHY.
    fn restart_policy(&self) -> &'static str;
```

`service/launchd.rs`: `fn restart_policy(&self) -> &'static str { "KeepAlive" }`
`service/systemd.rs`: `fn restart_policy(&self) -> &'static str { "Restart=always" }`

- [ ] **Step 4: Make `camp stop` refuse on a supervised camp (`cmd/stop.rs`)**

Rewrite the file's head and `run`, keeping the existing socket path byte-for-byte in `stop_over_socket`:

```rust
use anyhow::{Result, bail};

use crate::campdir::CampDir;
use crate::daemon::socket::{self, Request, Response};
use crate::service::{self, Supervisor, SystemProbe, SystemRunner};

/// `camp stop`: graceful daemon shutdown over the socket. Never auto-starts
/// (stopping nothing is an error, not a no-op).
///
/// On a SUPERVISED camp it refuses instead (operator decision, 2026-07-10):
/// the supervisor's KeepAlive / Restart=always would bring campd straight back,
/// so a socket stop that printed "campd stopped" would be a lie about the
/// verb's effect. Fail fast, name the remedy (invariants 3 and 5).
pub fn run(camp: &CampDir) -> Result<()> {
    let runner = SystemRunner;
    let probe = SystemProbe::new(&runner);
    let supervisor = service::host_supervisor(&probe, &runner)?;
    run_with(camp, supervisor.as_deref())
}

/// The testable core: the supervisor is injected, so both branches (supervised
/// and not) are unit-tested without a live service manager.
fn run_with(camp: &CampDir, supervisor: Option<&dyn Supervisor>) -> Result<()> {
    if let Some(supervisor) = supervisor
        && let Some(unit) = crate::cmd::service::managed_unit(supervisor, &camp.root)?
    {
        bail!(
            "campd for this camp is supervised by {} (unit {}, {}) — a socket stop would be \
             restarted immediately.\n       To stop it:      camp service stop\n       \
             To un-manage it: camp service uninstall",
            supervisor.name(),
            unit.name,
            supervisor.restart_policy()
        );
    }
    stop_over_socket(camp)
}

/// Unchanged from before this phase: the socket stop for an unsupervised camp.
fn stop_over_socket(camp: &CampDir) -> Result<()> {
    // A wedge is not "not running" (issue #55): the CampdUnresponsive
    // error already carries the truth (pid + kill -9 remedy) — layering
    // "campd is not running" over it would misdiagnose a live-but-stuck
    // daemon as an absent one.
    let response = socket::request(camp, &Request::Stop).map_err(|e| {
        if e.downcast_ref::<socket::CampdUnresponsive>().is_some() {
            e
        } else {
            e.context("campd is not running")
        }
    })?;
    match response {
        Response::Ok { .. } => {
            println!("campd stopped");
            Ok(())
        }
        other => bail!("unexpected response to stop: {other:?}"),
    }
}
```

(`let … && let …` chains are stable in edition 2024 / Rust 1.88+; this repo builds on 1.95. If you prefer, nest the two `if let`s — behavior is identical.)

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p camp --bins -- cmd::stop`
Expected: PASS.

Run: `cargo test -p camp --test cli_lifecycle --test daemon_lifecycle`
Expected: PASS — these drive `camp stop` on unmanaged temp camps, whose behavior is unchanged. (They now also shell one `id -u` per stop on macOS, via `supervisor_for`. Harmless.)

- [ ] **Step 6: Amend the feature design spec**

AGENTS.md: *"If implementation reality contradicts the spec, stop and update the spec via PR in the same change: spec and code never silently diverge."* Four edits to `docs/superpowers/specs/2026-07-10-campd-service-management-design.md` (and **only** that file — `docs/design/2026-07-05-gas-camp-design.md` and `contrib/launchd/` are Phase 4):

(a) In **§4 (Decision record)**, append:

```markdown
10. **`camp stop` refuses on a supervised camp** (operator, 2026-07-10). Always-on
    supervision (decision 2) means `KeepAlive` / `Restart=always` restarts campd
    immediately after a socket `Request::Stop` — so a `camp stop` that printed
    "campd stopped" would be a verb lying about its effect. It hard-errors instead,
    naming the supervisor, the unit, the always-on mechanism, and both remedies.
    On an unsupervised camp (container / CI / no manager) it is unchanged.
    Consequence: **`camp service stop` and `camp service start` join the §5 surface**
    (supervisor-level: `launchctl bootout` / `bootstrap`; `systemctl --user stop` /
    `start`), so the remedy the error names exists. Additive — nothing is removed.
    Rationale: invariant 5 (fail fast) + invariant 3 (nothing hidden).
```

(b) **Amend §4 decision 5 — it is the ONLY line in the spec that declares the verb surface.** (Verified: `grep -n 'install,uninstall' docs/superpowers/specs/2026-07-10-campd-service-management-design.md` returns exactly one hit, **line 97**. §5's intro carries no verb list at all — it reads *"A new subcommand group. Each operates on the resolved camp (`--camp` / `$CAMP_DIR` / walk-up), and delegates to the platform supervisor:"* — so there is nothing to change there. If you leave decision 5 alone, the spec ships declaring a five-verb surface while §4.10 and §5 describe seven: a self-contradiction, in the very file this PR amends *because* AGENTS.md forbids spec and code diverging.)

Replace the two lines at §4 decision 5:

```markdown
5. **`camp service {install,uninstall,status,restart,list}`** is the control
   surface; `list` is the "manage everything" view across all managed camps.
```

with:

```markdown
5. **`camp service {install,uninstall,status,restart,list,stop,start}`** is the
   control surface; `list` is the "manage everything" view across all managed
   camps. (`stop`/`start` were added by decision 10.)
```

(c) In **§5**, after the `list` bullet, **add** two bullets (§5 lists one bullet per verb; these are new, nothing is replaced):

```markdown
- **`stop`** — stop the supervised campd, leaving the unit INSTALLED
  (`launchctl bootout` / `systemctl --user stop`). This is what `camp stop`
  refuses in favor of on a supervised camp (decision 10).
- **`start`** — start a stopped but still-installed unit (`launchctl bootstrap` /
  `systemctl --user start`).
```

(d) In **§5**, the `status` bullet currently reads *"the unit's load/run state (wraps `launchctl print` / `systemctl --user status`)"*. The implementation uses `systemctl --user show --property=LoadState --property=ActiveState --property=SubState` — machine-readable, and it exits 0 even for a unit systemd has never heard of, which is what lets `status` be a state query rather than a failure path. Amend that parenthetical to name `systemctl --user show` so the spec matches the code (and list it in the PR body — see Deviations).

**Prove the amendment is complete** — no line anywhere may still declare the old surface:

```bash
grep -rn 'install,uninstall,status,restart,list}' docs/superpowers/specs/2026-07-10-campd-service-management-design.md
grep -rn 'systemctl --user status' docs/superpowers/specs/2026-07-10-campd-service-management-design.md
```

Expected: **no output** from either.

- [ ] **Step 7: Gates**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace`
Expected: all clean/PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/camp/src/cmd/stop.rs crates/camp/src/service docs/superpowers/specs/2026-07-10-campd-service-management-design.md
git commit -m "feat(stop): camp stop refuses on a supervised camp; camp service stop/start; spec §4.10"
```

---

## Task 5: Environment-aware `camp init`

Design §6 / decision §4.4. **This task also carries the mandatory test-suite sweep** — without it, `cargo test` on any macOS machine installs real LaunchAgents.

**Files:**
- Modify: `crates/camp/src/service/mod.rs` (add `ServiceChoice`, `Decision`, `decide` — PURE)
- Modify: `crates/camp/src/cmd/init.rs` (take a `ServiceChoice`; install / hand off / hard-fail)
- Modify: `crates/camp/src/main.rs` (`Init { service, no_service }` + dispatch)
- Modify: `crates/camp/tests/cli_init.rs` (add `--no-service` to its **8** camp-init calls; add the two new flag tests; add a `// not-camp:` marker to the **`git init -q`** on line 17)
- Modify (the sweep — add `--no-service` to every camp-init call): the 20 other files in the table at Step 6
- Modify: `crates/camp/tests/cli_export.rs` (add a `// not-camp:` marker to the **`bd init`** on line 221 — see Step 6; it is NOT a camp init)
- Create: `crates/camp/tests/no_bare_camp_init.rs` (the gate that makes the sweep permanent)

**Interfaces:**
- Consumes (Tasks 1–4): `service::{detect, supervisor_for, SystemProbe, SystemRunner, Manager}`, `cmd::service::{install, camp_binary}`.
- Produces: `service::ServiceChoice { Auto, Force, Skip }`; `service::Decision { Install(Manager), SkipByFlag, SkipNoManager, FailNoManager }`; `service::decide(choice, detected) -> Decision`; `cmd::init::run(camp_flag: Option<&Path>, choice: ServiceChoice) -> Result<()>`.

- [ ] **Step 1: Write the failing tests**

Add to `crates/camp/src/service/mod.rs` (a new test module at the bottom of the file) — the pure decision table design §9 requires:

```rust
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// Design §6: detection decides, the flags override. Six cells, all pinned.
    #[test]
    fn the_init_service_decision_is_a_pure_table() {
        // Default: a host with a manager gets a supervised campd…
        assert_eq!(
            decide(ServiceChoice::Auto, Some(Manager::Launchd)),
            Decision::Install(Manager::Launchd)
        );
        assert_eq!(
            decide(ServiceChoice::Auto, Some(Manager::Systemd)),
            Decision::Install(Manager::Systemd)
        );
        // …and a container/CI box gets a VISIBLE hand-off, not a failure.
        assert_eq!(decide(ServiceChoice::Auto, None), Decision::SkipNoManager);

        // --service forces it, and is a HARD ERROR where it cannot be honored.
        assert_eq!(
            decide(ServiceChoice::Force, Some(Manager::Systemd)),
            Decision::Install(Manager::Systemd)
        );
        assert_eq!(decide(ServiceChoice::Force, None), Decision::FailNoManager);

        // --no-service skips, manager or not.
        assert_eq!(decide(ServiceChoice::Skip, Some(Manager::Launchd)), Decision::SkipByFlag);
        assert_eq!(decide(ServiceChoice::Skip, None), Decision::SkipByFlag);
    }
}
```

Add to `crates/camp/tests/cli_init.rs`:

```rust
/// Design §6.4: `--no-service` skips the unit even on a desktop — and says so.
/// (Every OTHER init test in this repo passes --no-service too: a bare
/// `camp init` on a macOS host installs a REAL LaunchAgent and starts a
/// daemon. Unit CI must never do that.)
#[test]
fn init_no_service_skips_the_unit_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    camp()
        .current_dir(dir.path())
        .args(["init", "--no-service"])
        .assert()
        .success()
        .stdout(predicates::str::contains("service: skipped"));
    assert!(dir.path().join(".camp/camp.toml").exists());
}

/// The two flags are contradictory; clap rejects the pair (fail fast).
#[test]
fn init_rejects_service_and_no_service_together() {
    let dir = tempfile::tempdir().unwrap();
    camp()
        .current_dir(dir.path())
        .args(["init", "--service", "--no-service"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("cannot be used with"));
}
```

**Do NOT add a CLI test that runs a bare `camp init` (the `Auto` path) or `camp init --service`:** on macOS — a developer's machine and the `macos-latest` CI runner — both would install a real LaunchAgent and start a real daemon. The `Auto`/`Force` paths are covered by the pure `decide` table above and by the opt-in real-manager test in Task 6.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p camp --bins -- service::tests` — Expected: FAIL (`cannot find function \`decide\``).
Run: `cargo test -p camp --test cli_init` — Expected: FAIL (`error: unexpected argument '--no-service'`).

- [ ] **Step 3: Add the pure decision to `service/mod.rs`**

```rust
/// What the operator asked `camp init` to do about the host service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceChoice {
    /// Default: install where a manager exists; hand off where none does.
    Auto,
    /// `--service`: install, or fail loudly.
    Force,
    /// `--no-service`: never install.
    Skip,
}

/// What `camp init` will DO. Pure — `(choice, detection) → decision` — so
/// every environment is a unit test (design §9), and the IO-shaped half stays
/// a thin shell over a table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Install(Manager),
    SkipByFlag,
    SkipNoManager,
    FailNoManager,
}

pub fn decide(choice: ServiceChoice, detected: Option<Manager>) -> Decision {
    match (choice, detected) {
        (ServiceChoice::Skip, _) => Decision::SkipByFlag,
        (_, Some(manager)) => Decision::Install(manager),
        (ServiceChoice::Force, None) => Decision::FailNoManager,
        (ServiceChoice::Auto, None) => Decision::SkipNoManager,
    }
}
```

- [ ] **Step 4: Make `cmd/init.rs` environment-aware**

Replace its header and `run`:

```rust
use std::path::Path;

use anyhow::{Context, Result, bail};
use camp_core::ledger::Ledger;

use crate::service::{self, Decision, ServiceChoice, SystemProbe, SystemRunner};

/// Create a new camp: `<cwd>/.camp` by default, `--camp DIR` to choose. Then
/// (design §6) put its campd under the host's service manager where one
/// exists — `--service` forces it, `--no-service` skips it.
pub fn run(camp_flag: Option<&Path>, choice: ServiceChoice) -> Result<()> {
    let root = match camp_flag {
        Some(dir) => dir.to_path_buf(),
        None => std::env::current_dir()
            .context("cannot determine current directory")?
            .join(".camp"),
    };
    if root.join("camp.toml").exists() || root.join("camp.db").exists() {
        bail!("a camp already exists at {}", root.display());
    }
    std::fs::create_dir_all(&root).with_context(|| format!("cannot create {}", root.display()))?;

    let name = camp_name(&root);
    std::fs::write(
        root.join("camp.toml"),
        format!("# Gas Camp configuration (spec §7.1)\n[camp]\nname = \"{name}\"\n"),
    )
    .with_context(|| format!("cannot write camp.toml in {}", root.display()))?;

    Ledger::open(&root.join("camp.db"))?;

    // When the camp lives inside a git repo, keep its live runtime state
    // (ledger, socket, logs) out of git; `camp.toml` stays tracked (issue #35).
    crate::gitignore::ensure_camp_runtime_ignored(&root)?;

    println!("initialized camp at {}", root.display());

    // Design §6: detect a usable HOST service manager and act on the answer.
    // A container is not a failure — it is a different supervisor — so the
    // absent case is a VISIBLE hand-off on stderr, never a silent fallback.
    let runner = SystemRunner;
    let probe = SystemProbe::new(&runner);
    match service::decide(choice, service::detect(&probe)) {
        Decision::Install(manager) => {
            let supervisor = service::supervisor_for(manager, &probe, &runner)?;
            print!(
                "{}",
                crate::cmd::service::install(
                    supervisor.as_ref(),
                    &root,
                    &crate::cmd::service::camp_binary()?
                )?
            );
        }
        Decision::SkipByFlag => println!(
            "service: skipped (--no-service) — run `camp daemon --camp {}` under your supervisor",
            root.display()
        ),
        Decision::SkipNoManager => eprintln!(
            "camp: no host service manager detected (container/CI?) — run \
             `camp daemon --camp {}` under your supervisor (e.g. the container runtime)",
            root.display()
        ),
        Decision::FailNoManager => bail!(
            "--service: no host service manager detected (macOS launchd, or a reachable \
             systemd --user). The camp at {} was created, but NO unit was installed — run \
             `camp daemon --camp {}` under your supervisor instead.",
            root.display(),
            root.display()
        ),
    }
    Ok(())
}
```

(`camp_name` below it is unchanged.)

- [ ] **Step 5: Wire `main.rs`**

Replace the `Init` variant (lines 57-58):

```rust
    /// Create a new camp (./.camp by default; --camp DIR to choose the place)
    Init {
        /// Install and start a host service unit; a hard error when no host
        /// service manager is available (container/CI)
        #[arg(long, conflicts_with = "no_service")]
        service: bool,
        /// Do not install a host service unit (containers, CI, or by choice)
        #[arg(long = "no-service")]
        no_service: bool,
    },
```

Add the import next to `use campdir::CampDir;`:

```rust
use service::ServiceChoice;
```

Replace the `Command::Init` dispatch arm (line 439):

```rust
        Command::Init {
            service,
            no_service,
        } => {
            // Two bools at the CLI edge; ONE tri-state inside (clap already
            // rejected the contradictory pair).
            let choice = if service {
                ServiceChoice::Force
            } else if no_service {
                ServiceChoice::Skip
            } else {
                ServiceChoice::Auto
            };
            cmd::init::run(cli.camp.as_deref(), choice)
        }
```

- [ ] **Step 6: Sweep the test suite — every `camp init` gets `--no-service`**

**This is mandatory and it is not optional polish.** A bare `camp init` now installs a real host unit; a test that does it pollutes the developer's `~/Library/LaunchAgents` (and the `macos-latest` CI runner) and starts a real daemon.

**Read this before you touch anything: three `init` calls in this suite are NOT `camp init`, and rewriting them will break them.**

| Not a camp init | What it is | Do |
|---|---|---|
| `cli_export.rs:221` — `Command::new("bd") … .args(["init"])` | the **beads** CLI (`bd init`). `bd` would reject `--no-service` and blow the `assert!(init.status.success(), "bd init failed: …")` right below it — **silently**, because that test is `#[ignore]`d and `bd`-gated, so `cargo test --workspace` never runs it | **Leave the arguments alone.** Add only the marker comment below. |
| `cli_init.rs:17` — `Command::new("git") … .args(["init", "-q"])` | `git init -q`, inside the `git_init` helper | **Leave the arguments alone.** Add only the marker comment below. |
| the `vec!["init", "-b", "main"]` / `&["init", "-b", "main"]` forms in `daemon_dispatch.rs`, `daemon_patrol.rs`, `daemon_orders.rs`, `cli_claim_close.rs`, `e2e.rs` | `git init` through each file's `git(…)` helper | Nothing — they match neither the sweep nor the guard. |

The **camp-init** call sites — **33 calls across 21 files**, every one of them the single shape `.arg("init")`:

| File | Lines |
|---|---|
| `cli_backup.rs` | 22 |
| `cli_claim_close.rs` | 46 |
| `cli_create.rs` | 16, 71 |
| `cli_doctor.rs` | 15 |
| `cli_doctor_formula.rs` | 22, 44, 71 |
| `cli_event_emit.rs` | 24 |
| `cli_events.rs` | 17, 101 |
| `cli_export.rs` | **34 only** (line 221 is `bd init` — see above) |
| `cli_init.rs` | 41, 61, 79, 115, 140, 160, 193, 198 (line 17 is `git init` — see above) |
| `cli_lifecycle.rs` | 18 |
| `cli_ls.rs` | 17, 98 |
| `cli_order.rs` | 31 |
| `cli_rig.rs` | 64 |
| `cli_search.rs` | 16 |
| `cli_session.rs` | 53 |
| `cli_show.rs` | 16 |
| `cli_statusline.rs` | 29 |
| `daemon_lifecycle.rs` | 35 |
| `daemon_orders.rs` | 36 |
| `e2e_formula_valid.rs` | 17 |
| `plugin_hooks.rs` | 37 |
| **`cli_daemon_signal.rs`** | the `camp init` in `campd_stops_gracefully_on_sigterm` — **exists only after you rebase onto Phase 1 (PR #69).** Sweep it too; the guard test in Step 6b fails if you forget. |

There is exactly **one** rewrite shape:

```
.arg("init")   →   .args(["init", "--no-service"])
```

Then add the two `// not-camp:` markers (the guard test in Step 6b keys off them, and they stop the next reader from "fixing" these lines):

`crates/camp/tests/cli_export.rs:221`

```rust
        .args(["init"]) // not-camp: the beads CLI (`bd init`), not `camp init`
```

`crates/camp/tests/cli_init.rs:17`

```rust
        .args(["init", "-q"]) // not-camp: `git init`, not `camp init`
```

Then VERIFY the sweep by hand (the guard test in Step 6b is the permanent version):

```bash
grep -rn '\.arg("init")\|\.args(\["init"' crates/camp/tests/ \
  | grep -v -- '--no-service' | grep -v 'not-camp:' | grep -v 'real-manager:'
```

Expected: **no output.** Every camp init now passes `--no-service`; the two non-camp inits carry a `not-camp:` marker. (The third filter, `real-manager:`, matches nothing yet — Task 6 adds the one line that needs it, and this is the same command the final Verification checklist runs after Task 6.)

(Do **not** use a bare `grep '"init"'` — it also matches `git init` argument vectors and the `-m "init"` commit messages, which is exactly the false positive that would tempt you to break `bd init`.)

- [ ] **Step 6b: Make the sweep a GATE, not a convention**

A convention a future test can silently violate is not a safeguard. Create `crates/camp/tests/no_bare_camp_init.rs`:

```rust
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
//! The scan is LINE-ORIENTED, not a parser: an init call split across lines
//! (`.arg(\n    "init",\n)`) would slip past it. That is an accepted limit —
//! every call site in this suite is single-line, and the point is to stop the
//! easy, likely regression, not to be a Rust front end.

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
            if excused {
                continue;
            }
            violations.push(format!(
                "{}:{}: {}",
                path.file_name().unwrap().to_string_lossy(),
                i + 1,
                line.trim()
            ));
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
```

**Why a second marker rather than exempting the file.** Task 6's real-manager lifecycle test runs `camp init` with NO flags on purpose — that bare init IS the thing under test (design §6: on a host that has a manager, `camp init` installs the unit). Two escapes were considered and rejected: marking it `// not-camp:` would be a **lie** (it is the camp binary), and exempting `cli_service.rs` wholesale would blind the gate in the one file most likely to grow another bare init. A distinct marker, with its own documented precondition (`#[ignore]` + `CAMP_SERVICE_E2E=1`), keeps the exemption narrow, honest, and greppable:

```bash
grep -rn 'real-manager:' crates/camp/tests/
```

Expected: **exactly one line** — the `camp init` in `cli_service.rs`'s `#[ignore]`d lifecycle test. If that grep ever returns two, one of them needs justifying or fixing.

**Rebase note:** this gate is what catches Phase 1's `cli_daemon_signal.rs` automatically. After rebasing onto `main`, run it — if it fails, the fix is the one rewrite shape above.

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test -p camp --bins -- service::tests`
Expected: PASS (the decision table).

Run: `cargo test -p camp --test cli_init`
Expected: PASS — including the two new flag tests.

Run: `cargo test -p camp --test no_bare_camp_init`
Expected: PASS — the gate agrees the sweep is complete. (Sanity-check the gate itself once: temporarily revert one call site to `.arg("init")`, re-run, and watch it FAIL naming that file and line. Then restore it. A gate you have never seen fail is not a gate.)

Run: `cargo test --workspace`
Expected: PASS — the whole suite, with the sweep applied.

**Then verify by hand that no unit was installed by the test run** (this is the point of the sweep):

Run (macOS): `ls ~/Library/LaunchAgents | grep gascamp || echo "clean: no camp units"`
Run (Linux): `ls ~/.config/systemd/user 2>/dev/null | grep campd || echo "clean: no camp units"`
Expected: `clean: no camp units` (unless you personally installed one — in which case, exactly the one you installed, and no temp-directory camps).

- [ ] **Step 8: Gates**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace`
Expected: all clean/PASS.

- [ ] **Step 9: Commit**

```bash
git add crates/camp/src/service/mod.rs crates/camp/src/cmd/init.rs crates/camp/src/main.rs crates/camp/tests
git commit -m "feat(init): environment-aware camp init (--service/--no-service, visible container hand-off)"
```

---

## Task 6: The opt-in real-service-manager lifecycle test, the Makefile target, and the docs

Design §9's last obligation: "**`camp service` integration** (opt-in, local-only, like `make e2e`): install → status shows running → restart → uninstall, on the host's real service manager. Gated behind an env flag; not in unit CI."

**Files:**
- Modify: `crates/camp/tests/cli_service.rs` (add the `#[ignore]`d lifecycle test)
- Modify: `Makefile` (a `service-e2e` target)
- Modify: `README.md` (a `camp service` subsection)

**Interfaces:**
- Consumes: the whole `camp service` surface + `camp init` from Tasks 1–4, through the real `camp` binary.
- Produces: nothing consumed by later tasks (this is the last).

- [ ] **Step 1: Write the failing test**

Append to `crates/camp/tests/cli_service.rs`:

```rust
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Fail-loud env gate (no silent skip), mirroring `e2e.rs::require_e2e_env`.
fn require_service_e2e() {
    assert_eq!(
        std::env::var("CAMP_SERVICE_E2E").as_deref(),
        Ok("1"),
        "the service lifecycle test is opt-in and LOCAL-ONLY: set CAMP_SERVICE_E2E=1 \
         (use `make service-e2e`). It installs, starts, restarts and removes a REAL \
         unit in YOUR service manager."
    );
}

/// Always remove the unit, even if an assertion below blows up: a leaked
/// LaunchAgent/systemd unit would keep a campd alive on a temp directory.
///
/// Drop does NOT run on Ctrl-C or a hard kill. If you interrupt this test, the
/// unit survives — pointing at a tempdir that no longer exists, which the
/// supervisor will respawn-throttle forever. Clean it up by hand:
///
///     camp service list                       # find the orphan's camp id
///     # macOS:
///     launchctl bootout gui/$UID/com.gascamp.campd.<camp-id>
///     rm ~/Library/LaunchAgents/com.gascamp.campd.<camp-id>.plist
///     # Linux:
///     systemctl --user disable --now campd-<camp-id>.service
///     rm ~/.config/systemd/user/campd-<camp-id>.service && systemctl --user daemon-reload
struct Uninstall(PathBuf);

impl Drop for Uninstall {
    fn drop(&mut self) {
        let _ = std::process::Command::new(assert_cmd::cargo::cargo_bin("camp"))
            .args(["--camp"])
            .arg(&self.0)
            .args(["service", "uninstall"])
            .status();
    }
}

/// Block until campd answers on this camp's socket (test-side polling is
/// sanctioned for harnesses; campd itself never polls — invariant 1).
fn wait_for_campd(camp: &Path, want_listening: bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let out = std::process::Command::new(assert_cmd::cargo::cargo_bin("camp"))
            .args(["--camp"])
            .arg(camp)
            .args(["service", "status"])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        let listening = text.contains("campd: listening");
        if listening == want_listening {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "campd never reached listening={want_listening}; last status was:\n{text}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Design §9: the `camp service` lifecycle against the HOST's REAL service
/// manager — `camp init` installs → status shows running → list finds it →
/// restart → `camp stop` REFUSES (the 2026-07-10 operator ruling) → service
/// stop → service start → uninstall. OPT-IN and LOCAL-ONLY (`make
/// service-e2e`): it writes a real LaunchAgent / systemd user unit and starts a
/// real campd, then removes both. CI never runs it.
#[test]
#[ignore = "installs a REAL host service unit: run via `make service-e2e` (CAMP_SERVICE_E2E=1)"]
fn service_lifecycle_against_the_real_host_manager() {
    require_service_e2e();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".camp");

    // `camp init` with NO flag: the environment-aware default (design §6) —
    // on a host with a manager it installs and starts the unit itself. This
    // bare init is the THING UNDER TEST, and it is safe only because this test
    // is #[ignore]d AND gated on CAMP_SERVICE_E2E=1 (see the marker below; the
    // no_bare_camp_init gate documents when that marker is legitimate).
    let init = camp()
        .current_dir(dir.path())
        .args(["--camp"])
        .arg(&root)
        .arg("init") // real-manager: deliberate bare `camp init` — #[ignore]d + CAMP_SERVICE_E2E-gated
        .assert()
        .success();
    let _cleanup = Uninstall(root.clone());
    let init_out = String::from_utf8_lossy(&init.get_output().stdout).into_owned();
    assert!(
        init_out.contains("installed"),
        "on a host WITH a service manager, `camp init` installs the unit: {init_out}"
    );

    // The supervisor started campd; status shows BOTH truths.
    wait_for_campd(&root, true);
    let status = camp()
        .args(["--camp"])
        .arg(&root)
        .args(["service", "status"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let status = String::from_utf8_lossy(&status).into_owned();
    assert!(status.contains("running=true"), "the unit must be running: {status}");
    assert!(status.contains("campd: listening"), "campd must answer: {status}");

    // The fleet view finds this camp.
    let list = camp()
        .args(["service", "list"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let list = String::from_utf8_lossy(&list).into_owned();
    let canonical = std::fs::canonicalize(&root).unwrap();
    assert!(
        list.contains(&canonical.display().to_string()),
        "`camp service list` must name this camp: {list}"
    );

    // The post-upgrade cycle: campd comes back.
    camp()
        .args(["--camp"])
        .arg(&root)
        .args(["service", "restart"])
        .assert()
        .success();
    wait_for_campd(&root, true);

    // The operator's ruling (2026-07-10), end to end: `camp stop` REFUSES on a
    // supervised camp — and the remedy it names actually works.
    let refusal = camp()
        .args(["--camp"])
        .arg(&root)
        .arg("stop")
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    let refusal = String::from_utf8_lossy(&refusal).into_owned();
    assert!(refusal.contains("supervised by"), "{refusal}");
    assert!(refusal.contains("camp service stop"), "must name the remedy: {refusal}");
    wait_for_campd(&root, true); // …and the refusal stopped nothing.

    camp()
        .args(["--camp"])
        .arg(&root)
        .args(["service", "stop"])
        .assert()
        .success();
    wait_for_campd(&root, false); // the supervisor did NOT bring it back
    assert!(
        std::fs::canonicalize(&root).is_ok(),
        "a stopped camp is still a camp"
    );

    camp()
        .args(["--camp"])
        .arg(&root)
        .args(["service", "start"])
        .assert()
        .success();
    wait_for_campd(&root, true);

    // And it all comes out again.
    camp()
        .args(["--camp"])
        .arg(&root)
        .args(["service", "uninstall"])
        .assert()
        .success();
    wait_for_campd(&root, false);
    let after = camp()
        .args(["--camp"])
        .arg(&root)
        .args(["service", "status"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let after = String::from_utf8_lossy(&after).into_owned();
    assert!(after.contains("not installed"), "the unit must be gone: {after}");
}
```

- [ ] **Step 2: Run it to verify it fails (without the gate)**

Run: `cargo test -p camp --test cli_service -- --ignored`
Expected: FAIL — `assertion \`left == right\` failed` in `require_service_e2e` (`CAMP_SERVICE_E2E` is unset). That is the gate doing its job: the test refuses to touch your service manager unless you ask for it, out loud.

- [ ] **Step 3: Add the `Makefile` target**

After the `e2e:` target, add:

```makefile
# Opt-in `camp service` lifecycle against the HOST's REAL service manager
# (design §9). LOCAL-ONLY: it installs, starts, restarts, stops and removes a
# REAL launchd LaunchAgent / systemd --user unit for a throwaway camp, then
# cleans up. CI never runs it (the test is #[ignore]d AND gated on
# CAMP_SERVICE_E2E=1). Single-threaded: it manipulates your live service manager.
#
# If you INTERRUPT this run (Ctrl-C), the cleanup does not execute and a real
# unit is left behind pointing at a deleted tempdir — your supervisor will
# respawn-throttle it forever. Find and remove it:
#   camp service list
#   macOS: launchctl bootout gui/$UID/com.gascamp.campd.<camp-id> \
#            && rm ~/Library/LaunchAgents/com.gascamp.campd.<camp-id>.plist
#   Linux: systemctl --user disable --now campd-<camp-id>.service \
#            && rm ~/.config/systemd/user/campd-<camp-id>.service \
#            && systemctl --user daemon-reload
service-e2e:
	CAMP_SERVICE_E2E=1 cargo test -p camp --test cli_service -- --ignored --nocapture --test-threads=1
```

and extend the `.PHONY` line:

```makefile
.PHONY: install uninstall perf e2e service-e2e
```

- [ ] **Step 4: Run the real lifecycle test**

Run: `make service-e2e`
Expected: PASS. It installs a real unit for a temp camp, sees campd come up under the supervisor, restarts it, uninstalls it, and confirms campd is gone.

Then confirm it left nothing behind:

Run (macOS): `ls ~/Library/LaunchAgents | grep gascamp || echo "clean"`
Run (Linux): `ls ~/.config/systemd/user | grep campd || echo "clean"`
Expected: `clean` (or only units you installed yourself).

- [ ] **Step 5: Document `camp service` in the README**

In `README.md`, under `### campd & the daemon model` (around line 303-312, where `camp stop` is shown), add:

```markdown
#### Supervised campd — `camp service`

campd is a foreground, socket-serving process. On a desktop, `camp init` puts
it under the host's service manager, so it survives crashes, comes back at
login, and can be cycled after a binary upgrade:

    camp service install     # macOS: a KeepAlive LaunchAgent in ~/Library/LaunchAgents
                             # Linux: a Restart=always systemd --user unit
    camp service status      # the unit's load/run state + campd's liveness answer
    camp service restart     # cycle the daemon after upgrading the binary
    camp service stop        # stop campd (the unit stays installed)
    camp service start       # …and bring it back
    camp service uninstall   # stop, unload, remove the unit
    camp service list        # every camp with a managed unit, and its state

`camp init` does this for you when it detects a usable host service manager
(macOS launchd; Linux systemd `--user`). Where there is none — a container, a
CI box — it does not fail: it says so on stderr and hands off, and you run
`camp daemon --camp <dir>` under your own supervisor (the container runtime).
`camp init --no-service` skips the unit; `camp init --service` insists on one
and fails loudly if the host cannot provide it.

**On a supervised camp, `camp stop` refuses.** A supervised campd is kept alive
by its unit (`KeepAlive` / `Restart=always`), so a socket-level stop would be
undone by the supervisor moments later — and a verb that says "campd stopped"
about a daemon that is already coming back is lying. `camp stop` therefore
hard-errors and points you at `camp service stop` (stop it) or `camp service
uninstall` (un-manage it). On an unsupervised camp — a container, CI, a camp you
never installed a unit for — `camp stop` behaves exactly as it always has.

There is no registry file: the installed units ARE the registry, and
`camp service list` reads them.
```

- [ ] **Step 6: Gates**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace`
Expected: all clean/PASS. (`cargo test --workspace` does NOT run the `#[ignore]`d lifecycle test — that is the point.)

- [ ] **Step 7: Commit**

```bash
git add crates/camp/tests/cli_service.rs Makefile README.md
git commit -m "test(service): opt-in local-only camp service lifecycle against the real host manager"
```

---

## Verification (before opening the PR)

- [ ] `cargo fmt --all --check` → clean
- [ ] `cargo clippy --workspace --all-targets --all-features -- -D warnings` → clean
- [ ] `cargo test --workspace` → all pass, on this machine
- [ ] `cargo test -p camp --test no_bare_camp_init` → passes, **and you have seen it fail once** (Task 5, Step 7)
- [ ] `grep -rn '\.arg("init")\|\.args(\["init"' crates/camp/tests/ | grep -v -- '--no-service' | grep -v 'not-camp:' | grep -v 'real-manager:'` → empty
- [ ] `grep -rn 'real-manager:' crates/camp/tests/` → **exactly one line**: the deliberate bare `camp init` in `cli_service.rs`'s `#[ignore]`d, `CAMP_SERVICE_E2E`-gated lifecycle test
- [ ] `crates/camp/tests/cli_export.rs:221` still reads `.args(["init"])` — it is `bd init`, and it must NOT have gained `--no-service`
- [ ] `ls ~/Library/LaunchAgents | grep gascamp` (macOS) / `ls ~/.config/systemd/user | grep campd` (Linux) → nothing the test suite left behind
- [ ] `make service-e2e` → passes locally (the only place the real service manager is touched), and leaves no unit behind
- [ ] The design spec (`docs/superpowers/specs/2026-07-10-campd-service-management-design.md`) is amended in this same PR: §4.10 + §5's `stop`/`start` + the §5 `status` bullet
- [ ] CI green on BOTH `ubuntu-latest` and `macos-latest`
- [ ] The PR description names: the seam (three ports), the seven `camp service` verbs, the `camp init` flags, the `camp stop` refusal (operator decision, with the spec amendment), and the two spec-text divergences listed below.

---

## Deviations — READ AND RELAY (all must appear in the PR body)

**1. `camp stop` under supervision — RESOLVED by the operator (2026-07-10); implemented in Task 4.** The question this plan escalated in round 1 has been answered: **`camp stop` refuses loudly on a supervised camp**, `camp service stop` / `start` are added so the remedy exists, and **the feature design spec is amended in this PR** (§4.10 + §5). No open question remains. The v1 design doc (`docs/design/2026-07-05-gas-camp-design.md`) and the contradictory `contrib/launchd/README.md` ("Deliberately NO KeepAlive: `camp stop` must stay stopped") are **Phase 4's** reconciliation — the repo holds both statements for one phase, by design, and Phase 4 is the phase that fixes it.

**2. Two places where the implementation's TEXT diverges from spec §5's text (both amended in Task 4, Step 6; both belong in the PR body):**
- Spec §5 says `status` wraps `systemctl --user status`. The code uses `systemctl --user show --property=LoadState --property=ActiveState --property=SubState` — machine-readable, and it exits 0 even for a unit systemd has never heard of, which is exactly what lets `status` be a *state query* rather than a failure path. The spec bullet is amended to match.
- Spec §5's surface was {install, uninstall, status, restart, list}; it becomes {…, stop, start} (additive — nothing is removed). Amended.

**3. Deliberate scope handoffs (state them in the PR):**
- `contrib/launchd/`'s superseded example, the `README.md` quickstart/orders text that still describes on-demand campd, and the `docs/design/2026-07-05-gas-camp-design.md` §5/§9/§12 amendments are **Phase 4** (design §8; the orchestration guide assigns them there). Phase 2 adds only the new `camp service` README subsection.
- The CLI auto-start path (`daemon/autostart.rs`, `request_with_autostart`) is untouched: **Phase 3**. A camp with a managed unit simply never needs it, because campd is already up.

**4. `camp stop` now spawns a subprocess on every invocation.** `cmd::stop::run` resolves the host supervisor before it decides anything, which costs one `id -u` (macOS) or one `systemctl --user show-environment` (Linux) — the `CommandRunner` seam's only production calls. This is NOT an invariant-1 violation (no tick, no loop, no standing cost; the daemon is untouched), but it is a new process spawn on a hot CLI verb, and it is the price of the verb telling the truth about its effect. Worth one line in the PR body. If it ever shows up in a latency budget, the fix is to check for the unit file before probing the manager — but do not pre-optimize it here.

**5. `<camp-id>` is collision-*resistant*, not collision-*free*.** It is `<slug>-<32 bits of UUIDv5>`. Spec §5 says "collision-free"; 32 bits is not. Rather than widen the hash (a longer, less human-readable label), every verb that acts on a unit calls `managed_unit`, which asks the unit which camp it names and refuses loudly on a mismatch — so a collision is a loud error, never one camp's verb silently operating on another camp's daemon. That is the honest reading of "collision-free" in a source-of-truth-is-the-unit design, and it is worth one sentence in the PR body.

**6. Cross-phase (now enforced, not remembered):** Phase 1's `crates/camp/tests/cli_daemon_signal.rs` calls `camp init` with no flags. Task 5's sweep covers it and Task 5's `no_bare_camp_init` gate FAILS if it is missed — the hazard cannot ship silently.

**7. Task ordering note (not a deviation — for whoever executes out of order):** Task 2's `install` success message names `camp service stop`, which Task 3 adds. By the end of the phase this is correct; if Task 2 were ever shipped alone, that one string would name a verb that does not exist yet.

---

## Self-Review

**1. Spec coverage.**

| Spec requirement | Task |
|---|---|
| §5 `install` — launchd plist at `~/Library/LaunchAgents/com.gascamp.campd.<camp-id>.plist`, `ProgramArguments = camp daemon --camp <dir>`, `RunAtLoad` + `KeepAlive`, `launchctl bootstrap gui/$UID` | Task 2 (golden test + `load`) |
| §5 `install` — systemd `campd-<camp-id>.service`, `ExecStart=camp daemon --camp <dir>`, `Restart=always`, `systemctl --user enable --now` | Task 2 (golden test + `load`) |
| §5 `<camp-id>` — stable, collision-resistant, human-readable slug of the camp's absolute path (see Deviation 4) | Task 2 (`CampId::from_absolute`, three pinned tests; `managed_unit`'s identity check) |
| §5 `uninstall` — stop + unload + remove | Task 2 |
| §5 `status` — unit load/run state PLUS the campd liveness answer over the socket | Task 3 |
| §5 `restart` — `launchctl kickstart -k` / `systemctl --user restart` | Task 3 |
| §5 `stop` / `start` (added by the operator's 2026-07-10 ruling) | Task 3 (supervisor + flows), Task 4 (the spec amendment that records them) |
| §5 `list` — every managed camp, enumerated from the installed units (label prefix / `campd-*.service`), no registry file | Task 1 (`scan_units`, `installed`, `cmd::service::list`) |
| §5 generation is PURE and unit-tested; the launchctl/systemctl calls sit behind a seam | Tasks 1–2 (`Supervisor::unit_text`, `CommandRunner`) |
| §6 env-aware `camp init` — detect, install+start, or visible stderr hand-off; `--service` / `--no-service` | Task 5 |
| §9 unit generation tested with no live service manager | Task 2 |
| §9 environment detection tested via injected probes | Task 1 (`detect` + `FakeProbe`), Task 5 (`decide` table) |
| §9 opt-in, local-only, env-gated `camp service` integration test, not in unit CI | Task 6 |
| §4.5 the control surface (now seven verbs) | Tasks 1–3 |
| §4.10 (new) `camp stop` refuses on a supervised camp | Task 4 (code + the spec amendment that records the decision) |
| §10 no new event types; nothing hidden; fail fast | Global Constraints; no ledger writes anywhere in this phase; `unit_safe_str` is the fail-fast gate on the one place a lossy conversion could have hidden |

Out of scope by construction (and stated): §4.3/§8 auto-start removal (Phase 3); §7 SIGTERM (Phase 1); §7 container reference + the §8 *v1-design-doc* amendments (Phase 4). The **feature spec** amendment is in scope and lands in Task 4.

**2. Placeholder scan.** No TBD / "add error handling" / "similar to Task N" / "write tests for the above". Every code step carries its code; every test step carries the assertions; every run step names the command and the expected result. The one mechanical bulk edit (Task 5, Step 6) gives the exact file list, the single rewrite shape, the three lines that must NOT be rewritten and why, a sound verification grep, and a test that enforces it forever.

**3. Type consistency.** `CampId` is `from_slug` / `for_camp` / `from_absolute` everywhere (never `parse`, never `new`). `Supervisor` grows in exactly four steps — Task 1: `name`, `unit_path`, `parse_camp_root`, `state`, `installed`; Task 2: `unit_name`, `unit_text`, `reload_units`, `load`, `unload`; Task 3: `restart`, `stop`, `start`; Task 4: `restart_policy` — and every implementation and call site uses those names. **`unit_text` takes `&str, &str`** (never `&Path`) in the trait, both impls, and all four of its tests — that is what makes the B2 lossy conversion structurally impossible. `RunOutcome { code, stdout, stderr }` + `success()` is used identically by `run_checked`, `Launchd::state` and `Systemd::state`. `cmd::service::{install, uninstall, status, restart, stop, start, list}` all return `Result<String>` (the report) with `run_*` wrappers printing it; `status` and `list` take `Option<&dyn Supervisor>` (a container has none), the five mutating flows take `&dyn Supervisor` (`require_supervisor` has already failed loudly if there is none). `managed_unit` / `require_managed_unit` / `ManagedUnit { id, name, path }` are the single "is this camp managed?" answer, used by `install`, `uninstall`, `status`, `restart`, `stop`, `start` **and** `cmd::stop::run_with`. `service::{ServiceChoice, Decision, decide}` are used with the same variant names in `service/mod.rs`, its tests, `cmd/init.rs` and `main.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The reference container, end to end (feature design §7/§9; v1 spec §5).
//!
//! OPT-IN and LOCAL-ONLY, exactly like `make e2e` and `make service-e2e`: the
//! measured test is `#[ignore]`d AND requires CAMP_CONTAINER_E2E=1, so
//! `cargo test --workspace` and CI never build or run Docker. Run it with
//! `make container-smoke`. It needs a working `docker` on PATH.
//!
//! What it proves about contrib/docker/:
//!   1. the image builds, and the entrypoint's `camp init --no-service
//!      --exists-ok` is a NO-OP on an already-initialized camp — the restart
//!      path, which a bare `camp init` would crash-loop on;
//!   2. campd is the container's main process and answers on the in-container
//!      socket: `docker exec <c> camp sling "…"` creates a bead, campd
//!      dispatches a worker, and the worker claims and closes it — the whole
//!      round trip, inside the container;
//!   3. `docker stop` is graceful: SIGTERM reaches campd (that is what `exec`
//!      in the entrypoint buys), the ledger gets `campd.stopped`, the socket is
//!      unlinked, and the container exits 0 — FAST, not after the 10 s SIGKILL
//!      grace. This is Phase 1's payoff, measured.
//!
//! The worker is a four-line POSIX-sh fake wired in through `[dispatch]
//! command` — visible config, not a fallback — so the image needs no `claude`
//! and this test spends no API money.
//!
//! Two environment notes, stated rather than assumed silently:
//!   - The fixture is streamed onto the camp volume with `docker cp` through a
//!     short-lived prep container — deliberately NOT a host bind mount. A bind
//!     mount of a `tempfile::tempdir()` path depends on the daemon's file
//!     sharing: on Docker Desktop for macOS, `$TMPDIR` (`/var/folders/…`, real
//!     path `/private/var/folders/…`) is outside the default shared set, and the
//!     mount then silently comes up EMPTY rather than failing — a test that
//!     fails for a reason that is not the code's fault, in a way that looks like
//!     the code's fault. `docker cp` goes over the Docker API and needs no
//!     sharing at all, so this test runs the same on Linux and on Desktop.
//!   - The `Drop` guard removes both containers, the volume AND the image, so a
//!     run leaves nothing behind.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const IMAGE: &str = "gascamp-container-smoke:test";
const CONTAINER: &str = "gascamp-container-smoke";
/// The short-lived container that holds the volume while the fixture is copied
/// onto it. Named (not `--rm` anonymous) so the Drop guard can always reap it.
const PREP: &str = "gascamp-container-smoke-prep";
const VOLUME: &str = "gascamp-container-smoke-camp";
/// The uid the image's `camp` user owns the camp with (Dockerfile: `useradd
/// --uid 10001`). `docker cp` lands files as root, so the prep step hands them
/// back to that uid — campd runs as it and must be able to read and exec them.
const CAMP_UID: &str = "10001";
/// campd must answer `docker stop`'s SIGTERM well inside the 10 s grace a plain
/// `docker stop` gives it; an ignored SIGTERM shows up as ~10 s + exit 137,
/// which is the failure this bound catches. (Unrelated to compose.yaml's
/// `stop_grace_period: 30s`, which is a *ceiling* for a slow real shutdown, not
/// a target — this test does not use compose.)
const GRACEFUL_STOP_MAX: Duration = Duration::from_secs(5);

fn repo_root() -> PathBuf {
    // crates/camp/ -> crates/ -> repo root
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .unwrap()
}

fn require_opt_in() {
    assert_eq!(
        std::env::var("CAMP_CONTAINER_E2E").as_deref(),
        Ok("1"),
        "the container smoke test is opt-in and LOCAL-ONLY: set CAMP_CONTAINER_E2E=1 \
         (use `make container-smoke`). It builds a Docker image and runs a container."
    );
}

/// Run docker and return the outcome. Failure is the CALLER's to judge.
fn docker(args: &[&str]) -> std::process::Output {
    Command::new("docker")
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("docker {args:?} could not be run ({e}) — is docker on PATH?"))
}

/// Run docker and fail loudly on a non-zero exit. Returns stdout.
fn docker_ok(args: &[&str]) -> String {
    let out = docker(args);
    assert!(
        out.status.success(),
        "docker {args:?} failed ({}):\n--- stdout ---\n{}\n--- stderr ---\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Pre-run cleanup ONLY: a leftover container/volume from an aborted run must
/// not fail the next one, and "there was nothing to remove" is a fine outcome.
/// Every assertion in the test proper goes through docker_ok.
fn docker_cleanup(args: &[&str]) {
    let _ = docker(args);
}

/// Removes the container, the volume AND the image even when the test panics —
/// a test suite that silts up the developer's Docker with a stray image per run
/// is a test suite people stop running.
struct Cleanup;
impl Drop for Cleanup {
    fn drop(&mut self) {
        docker_cleanup(&["rm", "-f", CONTAINER]);
        docker_cleanup(&["rm", "-f", PREP]);
        docker_cleanup(&["volume", "rm", "-f", VOLUME]);
        docker_cleanup(&["image", "rm", "-f", IMAGE]);
    }
}

/// The camp the container will serve, written on the host and copied into the
/// volume by a prep container. One rig (a plain directory — the `dev` agent
/// pins `isolation: none`, so no worktree and no git repo is needed), one
/// agent, and a worker script that speaks the camp worker contract.
fn write_fixture(dir: &Path) {
    std::fs::write(
        dir.join("camp.toml"),
        "[camp]\nname = \"smoke\"\n\n\
         [[rigs]]\nname = \"demo\"\npath = \"/camp/rig\"\nprefix = \"d\"\n\n\
         [dispatch]\nmax_workers = 1\ncommand = \"/camp/worker.sh\"\ndefault_agent = \"dev\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("dev.md"),
        "---\nname: dev\nisolation: none\n---\nDo the work.\n",
    )
    .unwrap();
    // campd sets CAMP_DIR/CAMP_BEAD/CAMP_SESSION in the worker's env and passes
    // claude-style argv, which a fake worker ignores (same contract as
    // tests/fake-agent.sh). claim -> close is the whole worker.
    std::fs::write(
        dir.join("worker.sh"),
        "#!/bin/sh\nset -eu\n\
         /usr/local/bin/camp claim \"$CAMP_BEAD\" --session \"$CAMP_SESSION\"\n\
         /usr/local/bin/camp close \"$CAMP_BEAD\" --outcome pass --reason \"container smoke\"\n",
    )
    .unwrap();
}

fn events(container: &str) -> Vec<serde_json::Value> {
    docker_ok(&["exec", container, "camp", "events", "--json"])
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// Harness-side waiting (camp itself never polls; tests may). Panics with the
/// container's logs so a failure is diagnosable without a rerun.
fn wait_for(what: &str, timeout: Duration, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    loop {
        if done() {
            return;
        }
        if Instant::now() > deadline {
            let logs = docker(&["logs", CONTAINER]);
            panic!(
                "timed out after {timeout:?} waiting for {what}\n--- container stdout ---\n{}\n\
                 --- container stderr ---\n{}",
                String::from_utf8_lossy(&logs.stdout),
                String::from_utf8_lossy(&logs.stderr),
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
#[ignore = "opt-in, local-only: builds and runs Docker (make container-smoke)"]
fn reference_container_serves_a_camp_and_stops_gracefully() {
    require_opt_in();
    let root = repo_root();
    let fixture = tempfile::tempdir().unwrap();
    write_fixture(fixture.path());

    docker_cleanup(&["rm", "-f", CONTAINER]);
    docker_cleanup(&["volume", "rm", "-f", VOLUME]);
    let _cleanup = Cleanup;

    // 1. The image builds from the repo root (the Dockerfile compiles the
    //    workspace; the build context is therefore the repo, not contrib/).
    //    `-f` is resolved by the docker CLI against ITS OWN cwd — and cargo runs
    //    an integration test with cwd = the package dir (crates/camp), not the
    //    repo root — so the dockerfile path must be absolute, not relative.
    let dockerfile = root.join("contrib").join("docker").join("Dockerfile");
    docker_ok(&[
        "build",
        "-f",
        dockerfile.to_str().unwrap(),
        "-t",
        IMAGE,
        root.to_str().unwrap(),
    ]);

    // 2. Prepare the camp ON the volume, before the container ever starts, so
    //    that the entrypoint meets an EXISTING camp — the restart path.
    docker_ok(&["volume", "create", VOLUME]);
    let vol_mount = format!("{VOLUME}:/camp");
    docker_ok(&[
        "run",
        "--rm",
        "-v",
        &vol_mount,
        "--entrypoint",
        "camp",
        IMAGE,
        "init",
        "--camp",
        "/camp",
        "--no-service",
    ]);

    // The fixture goes onto the volume through a prep container over the Docker
    // API (`docker cp`), never a host bind mount — see the module docs: a bind
    // mount of $TMPDIR silently mounts EMPTY on Docker Desktop for macOS.
    docker_ok(&[
        "run",
        "-d",
        "--name",
        PREP,
        "-v",
        &vol_mount,
        "--entrypoint",
        "sh",
        IMAGE,
        "-c",
        "sleep 300",
    ]);
    docker_ok(&["exec", PREP, "sh", "-c", "mkdir -p /camp/agents /camp/rig"]);
    for (host, dest) in [
        (fixture.path().join("camp.toml"), "/camp/camp.toml"),
        (fixture.path().join("dev.md"), "/camp/agents/dev.md"),
        (fixture.path().join("worker.sh"), "/camp/worker.sh"),
    ] {
        let target = format!("{PREP}:{dest}");
        docker_ok(&["cp", host.to_str().unwrap(), &target]);
    }
    // `docker cp` lands files as root; campd runs as uid 10001 and must be able
    // to read the config and EXEC the worker. `-u 0` because the image's own
    // USER cannot chown root's files.
    let fix_perms = format!("chown -R {CAMP_UID}:{CAMP_UID} /camp && chmod 0755 /camp/worker.sh");
    docker_ok(&["exec", "-u", "0", PREP, "sh", "-c", &fix_perms]);
    docker_ok(&["rm", "-f", PREP]);

    // 3. Start the real thing: the image's own entrypoint, nothing overridden.
    docker_ok(&["run", "-d", "--name", CONTAINER, "-v", &vol_mount, IMAGE]);

    // 4. The entrypoint's init found the camp and said so instead of dying,
    //    and campd came up and announced its socket — both on stdout.
    wait_for(
        "campd to announce its socket",
        Duration::from_secs(60),
        || {
            let logs = docker(&["logs", CONTAINER]);
            String::from_utf8_lossy(&logs.stdout).contains("campd listening on /camp/campd.sock")
        },
    );
    let logs = docker(&["logs", CONTAINER]);
    let stdout = String::from_utf8_lossy(&logs.stdout).into_owned();
    assert!(
        stdout.contains("already exists"),
        "the entrypoint's `camp init --exists-ok` must be a no-op success on the existing \
         camp (a bare `camp init` would exit 1 and crash-loop the container); logs were:\n{stdout}"
    );

    // 5. Drive it the documented way: the CLI is a pure socket client, and
    //    `docker exec` puts it on the same side of the socket as campd.
    //    ($CAMP_DIR is set in the image, so no --camp is needed.)
    docker_ok(&["exec", CONTAINER, "camp", "sling", "smoke: dispatch a bead"]);

    // 6. campd dispatched it and the in-container worker closed it.
    wait_for(
        "the bead to be dispatched and closed",
        Duration::from_secs(60),
        || {
            let evs = events(CONTAINER);
            evs.iter().any(|e| e["type"] == "bead.claimed")
                && evs.iter().any(|e| e["type"] == "bead.closed")
        },
    );
    let evs = events(CONTAINER);
    let closed = evs.iter().find(|e| e["type"] == "bead.closed").unwrap();
    assert_eq!(
        closed["data"]["outcome"], "pass",
        "the worker closed the bead pass; events were: {evs:#?}"
    );

    // 7. `docker stop` = SIGTERM to campd (tini forwards it to its only child,
    //    which the entrypoint exec'd). Graceful means: quick, exit 0, and the
    //    shutdown is IN THE LEDGER.
    let stop_started = Instant::now();
    docker_ok(&["stop", CONTAINER]);
    let stop_took = stop_started.elapsed();
    assert!(
        stop_took < GRACEFUL_STOP_MAX,
        "docker stop took {stop_took:?} — campd did not answer SIGTERM promptly (an ignored \
         SIGTERM shows up as the full 10 s grace, then SIGKILL)"
    );

    let code = docker_ok(&["inspect", "-f", "{{.State.ExitCode}}", CONTAINER]);
    assert_eq!(
        code.trim(),
        "0",
        "the container must exit 0 on SIGTERM (137 = SIGKILL after the grace period)"
    );

    // The ledger and the socket, read from the volume after the fact.
    let after = docker_ok(&[
        "run",
        "--rm",
        "-v",
        &vol_mount,
        "--entrypoint",
        "camp",
        IMAGE,
        "events",
        "--camp",
        "/camp",
        "--json",
    ]);
    assert!(
        after.lines().any(|l| l.contains("\"campd.stopped\"")),
        "SIGTERM must append campd.stopped to the ledger; events were:\n{after}"
    );
    docker_ok(&[
        "run",
        "--rm",
        "-v",
        &vol_mount,
        "--entrypoint",
        "sh",
        IMAGE,
        "-c",
        "test ! -e /camp/campd.sock",
    ]);
}

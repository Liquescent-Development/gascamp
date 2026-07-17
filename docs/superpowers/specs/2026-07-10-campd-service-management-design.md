# campd Service Management — Supervised Daemon, Pluggable Supervision — Design

> **Status:** design approved 2026-07-10 (brainstorming, operator-directed).
> Next step: implementation plan (superpowers:writing-plans). This document is
> the spec; it does not touch code. The authoritative v1 spec
> (`docs/design/2026-07-05-gas-camp-design.md`) is amended by the
> implementation PR (§8), per AGENTS.md — spec and code never silently diverge.

## 1. Problem

campd has no service management. Today it **auto-starts on demand**: any CLI
verb that needs the daemon spawns a detached `campd` if the socket is absent
(spec §5, `autostart::request_with_autostart`). It is stopped manually
per-camp (`camp stop`), and after a binary upgrade the running daemon keeps
executing the *old* binary until someone manually restarts it. There is no OS
integration, no crash-restart, no single place to see or manage the daemons,
and — because a camp can be created per-project (repo-local `.camp/`) — a user
with many projects can accumulate many unmanaged, long-lived `campd` processes.
This is a maintenance nightmare at any real scale, and it is shipped to every
camp user.

Observed this session: a `campd` (pid 47117) kept running the pre-upgrade
binary for hours after the binary was rebuilt; there was no command to
"restart the daemon" beyond `camp stop` + a manual re-trigger, per camp.

## 2. Goals / non-goals

**Goals**

- First-class, **cross-platform** daemon supervision, shipped for every user:
  macOS (launchd), Linux (systemd `--user`), **containers** (the container
  runtime), and CI/bare boxes (manual `camp daemon`).
- **One conceptual path:** campd is a *supervised, foreground, long-lived
  process*; the CLI is a *pure socket client* in every environment. No
  CLI-magic-spawn, no on-demand-vs-keepalive mode fork.
- **Managed at `camp init`:** creating a camp establishes its supervised
  daemon where a host supervisor exists.
- **Config edits hot-reload live** (already true — documented and relied on).
- Crash-restart and clean binary-upgrade cycling via native tooling.

**Non-goals**

- **Not** a single global daemon serving all camps — per-camp isolation (one
  ledger, crash-only) is preserved; one camp's crash never touches another.
- **Not** socket activation. We choose **always-on** (see §4, decision 2):
  simpler, keeps scheduled orders firing, and is the native container shape.
- **Not** ephemeral one-shot container runs (`docker run camp sling …` that
  exits): camp is durable async work; the target is a long-lived camp service.
- No new network transport — campd stays a unix-domain socket under `<camp>/`.

## 3. Core model: campd is a supervised foreground process

The single primitive is **`camp daemon`** — a foreground, long-lived,
socket-serving process (it already exists; the OS units and the container
entrypoint all run exactly this). Every environment runs the same primitive;
only *who supervises it* differs. Supervision is **pluggable**:

| Environment | Supervisor | How campd starts |
|---|---|---|
| macOS desktop | launchd (LaunchAgent, `KeepAlive`) | `camp service install` runs `camp daemon`; auto at `camp init` |
| Linux desktop/server (systemd user session) | systemd `--user` (`Restart=always`) | `camp service install` runs `camp daemon`; auto at `camp init` |
| **Container** | the container runtime (Docker `restart:`, K8s) | `camp daemon` **is** the container's main process |
| CI / bare box | you | `camp daemon` run directly |

**The CLI is a pure socket client in every environment.** It never spawns
campd; the supervisor keeps campd alive. A dead socket is a loud, actionable
fault (naming the pid from the ledger's `campd.started`, and pointing at
`camp service status`), never a silent respawn. This *removes* today's
CLI-self-spawn auto-start and is the "one path" the design commits to: the
container runtime is simply another supervisor slotting into the same slot as
launchd/systemd.

**Always-on, within the idle budget.** A supervised campd runs continuously.
Invariant 1 ("idle campd < 20 MB RSS, 0.0% CPU") is about the daemon's idle
*footprint*, which always-on still meets (campd sleeps on OS events; no ticks).
This is a deliberate, operator-directed evolution of §5 from "on-demand, zero
processes when idle" to "supervised always-on, zero idle cost" — chosen for
manageability, reliable order firing, and native container fit.

## 4. Decision record

1. **Supervised-foreground model, pluggable supervision.** `camp daemon` is
   the one primitive; the supervisor is environment-provided. Not a global
   multi-camp daemon (breaks isolation/crash-only), not socket activation.
2. **Always-on, not socket activation.** Socket activation would give
   zero-idle-pids but leaves campd dead when idle, so scheduled orders (§9)
   would not fire, and macOS `launch_activate_socket` needs extra FFI.
   Always-on (KeepAlive / `Restart=always` / container runtime) keeps orders
   firing, is simpler, and is the native container shape. Idle cost stays 0%
   CPU / <20 MB, so invariant 1 holds.
3. **CLI is a pure client; the on-demand CLI auto-start is removed.** One
   path. `camp daemon` + the supervisor are the only way campd runs.
4. **`camp init` is environment-aware.** Detect a usable host service manager
   → install + start the unit (default). None detected (container/CI/minimal)
   → do not fail; print a clear hand-off (run `camp daemon` under your
   runtime). Flags `--service` (force; error if unavailable) / `--no-service`.
5. **`camp service {install,uninstall,status,restart,list,stop,start}`** is the
   control surface; `list` is the "manage everything" view across all managed
   camps. (`stop`/`start` were added by decision 10.)
6. **campd handles SIGTERM/SIGINT → graceful shutdown**, identical to the
   socket `Request::Stop`. Every supervisor stops a service with SIGTERM, so
   this makes campd well-behaved everywhere. (It already reaps its worker
   children via the SIGCHLD self-pipe.)
7. **Container is a first-class supervisor.** Ship a reference Docker setup.
8. **Config hot-reload already exists** (campd watches `camp.toml` via
   `notify`, re-parses, swaps into orders/dispatcher/graph, emits
   `config.changed`); optionally extend it to patrol stall-timer config
   (currently startup-only).
9. **Per-camp isolation preserved; usage steered to standalone-multi-rig.**
   Since always-on = one process per camp, document one standalone camp with
   many `camp rig add` repos as the recommended pattern to bound daemon count;
   repo-local `.camp/` still works and costs one daemon each.
10. **`camp stop` refuses when the supervisor would put campd straight back**
    (operator, 2026-07-10). Always-on supervision (decision 2) means `KeepAlive` /
    `Restart=always` restarts campd immediately after a socket `Request::Stop` — so a
    `camp stop` that printed "campd stopped" would be a verb lying about its effect.
    It hard-errors instead, naming the supervisor, the unit, the always-on mechanism,
    and both remedies. On an unsupervised camp (container / CI / no manager) it is
    unchanged.

    The refusal is keyed on **"will this supervisor restart campd?"**, which each
    supervisor answers for itself — *not* on the unit file existing, and *not* on a
    single shared "loaded" flag, because the two managers do not mean the same thing
    by it. launchd: a **bootstrapped** label, since `KeepAlive` is unconditional.
    systemd: a unit whose `ActiveState` is **`active`, `activating` or `reloading`**,
    since `Restart=always` acts only on a running unit — `LoadState=loaded` is still
    true of an inactive, dead or failed unit and means only that the unit file parsed.
    (`activating` covers systemd's `auto-restart` sub-state: that IS the crash-loop,
    and systemd will put campd back.) When the answer is no, nothing will
    undo a socket stop, so `camp stop` performs it: it is then the honest verb for a
    campd the supervisor does not own. (A refusal keyed on the unit file, or on
    `loaded`, leaves such a campd un-stoppable by any verb — `camp stop` refusing and
    `camp service stop` unable to stop what it never started.)

    Consequence: **`camp service stop` and `camp service start` join the §5 surface**
    (supervisor-level: `launchctl bootout` / `bootstrap`; `systemctl --user stop` /
    `start`), so the remedy the error names exists. Additive — nothing is removed.
    Rationale: invariant 5 (fail fast) + invariant 3 (nothing hidden).

11. **Every verb that hands campd to (or takes it from) the supervisor verifies its
    own effect over the socket.** `camp service install` / `start` refuse when a campd
    already holds the camp's socket: a supervised campd cannot take over a live socket
    (§5 bind rules — it exits), and `KeepAlive` / `Restart=always` would then respawn
    it forever while the verb reported "now supervised". `camp service stop` re-checks
    the socket after stopping the unit and refuses to report a stop that did not
    happen; `uninstall` reports a campd that survives it. No verb may take its own
    word for its effect (invariants 3 and 5).

    **Stopping is ASYNCHRONOUS on launchd, and the verification must allow for it.**
    `launchctl bootout` returns 0 while campd is still running its graceful exit —
    the socket goes quiet ~8-18 ms later and the process lingers to ~760 ms
    (measured, macOS). `systemctl --user stop` blocks until the process is gone, so
    Linux does not show this and macOS always does. That asymmetry is a trap, and it
    has now bitten twice: first as `loaded` (which means "bootstrapped" to launchd
    but merely "the unit file parsed" to systemd — decision 10), and then here, where
    a verify step that probed the socket the instant `bootout` returned met a campd
    part-way through the shutdown it had just ordered and called it a fault. `camp
    service stop` exited 1 with a scary error while the unit was, in fact, stopped —
    the check that exists to stop verbs lying about their effect, lying about its
    own, pointing the other way.

    So a campd that accepts and then closes without answering is a THIRD state, not
    a fault: to a verb that just ordered a stop it is success in progress. The
    verification waits it out, bounded, and only concludes when the socket is quiet
    (stopped), a campd answers properly (an orphan the manager does not own — the
    fault this check exists to catch), or the window expires. **Do not "simplify"
    that wait away**, and do not assume a supervisor's stop is synchronous because
    one of them is: the last author to reason that way ("a successful `bootout`
    blocks until the process is gone") shipped this bug, having been unable to run
    the real-manager test that catches it instantly.

12. **Both units name their stop shape: systemd `KillMode=process`, launchd
    `AbandonProcessGroup=true`** (2026-07-16, issue #119). A supervisor's stop is the
    SIGTERM shape (decision 6, §7): campd alone is signalled, appends
    `campd.stopped`, unlinks the socket and exits; the in-flight `claude -p` workers
    survive, are reparented, and `patrol::adopt` re-adopts them at the next start.
    **Neither platform gives that shape for free — each sweeps campd's children by
    default, by a different mechanism — so each needs an explicit directive.**

    - **systemd.** The default `KillMode=control-group` SIGTERMs, then SIGKILLs,
      **every** process in the unit's cgroup, so `camp service stop` on a busy camp
      abandons in-flight work (reconciled as `session.crashed`) rather than letting it
      reparent. `process` signals the main pid only. `mixed` was rejected: it SIGTERMs
      only the main process, but once campd exits it SIGKILLs whatever remains in the
      cgroup after `TimeoutStopSec` — the workers. That is the same bug, arriving late
      rather than immediately.
    - **launchd.** `man 5 launchd.plist`, `AbandonProcessGroup`: *"When a job dies,
      launchd kills any remaining processes with the same process group ID as the job.
      Setting this key to true disables that behavior."* It defaults to **false**, so
      the sweep is **on**; `daemon/spawn.rs` sets no `process_group`, so workers inherit
      campd's pgid. `launchctl bootout` therefore killed every in-flight worker —
      systemd's `control-group` bug wearing a different hat. `AbandonProcessGroup=true`
      turns the sweep off.

    So parity is something camp **does**, not something it inherits.

    **This is where the first attempt at this decision went wrong, and the error is
    worth keeping.** It shipped `KillMode=process` (correct) justified by *"launchd is
    already at parity because macOS has no cgroup to sweep"* — a non-sequitur that
    read as a verified fact. macOS has no cgroup; it sweeps the **process group**
    instead. The claim was checked by reading `stop` → `bootout <service target>` and
    reasoning from the absence of one mechanism to the absence of **any** mechanism.
    **Disproved empirically on a macOS host (2026-07-16):** a LaunchAgent mirroring
    camp's real plist (RunAtLoad + KeepAlive, no `AbandonProcessGroup`), whose job
    spawned a child the way `spawn.rs` does, `launchctl bootout` → the child was
    **dead**; with `AbandonProcessGroup=true`, the same probe left it **alive** (pid
    reparented to 1, job gone). The lesson is procedural: macOS was the one platform
    that *was* testable on the dev host, and it was the one nobody tested. **When a
    platform is reachable, the claim about it is an experiment, not an argument.**

    **The directives govern the CRASH path too, not only `stop`** — `Restart=always` /
    `KeepAlive` tear the job down the same way. Previously a campd crash swept the
    cgroup (or the process group) and killed the workers with it; now they survive and
    `RestartSec=1` brings campd back to re-adopt them. That is safe rather than a new
    hazard because adoption is **pid-keyed** (`patrol::adopt_from_row(row, pid)`) and
    reads no ppid — a reparented worker is found exactly as before.

    This decision was **forward-flagged by Phase 1 and then dropped**
    (`docs/superpowers/plans/2026-07-10-campd-service-phase-1-sigterm.md:34`: "Phase 2
    must choose `KillMode` deliberately … rather than inheriting it silently"). Phase
    2 never made it, and the unit shipped inheriting a default that contradicted the
    design it was implementing — and the launchd side was never flagged at all. Named
    here so it stays decided: **an unnamed `KillMode`, or an unnamed
    `AbandonProcessGroup`, is not a default — it is an unmade decision.**

## 5. `camp service` — the control surface

A new subcommand group. Each operates on the resolved camp (`--camp` /
`$CAMP_DIR` / walk-up), and delegates to the platform supervisor:

- **`install`** — generate the unit and load it: macOS → a LaunchAgent plist
  at `~/Library/LaunchAgents/com.gascamp.campd.<camp-id>.plist`
  (`ProgramArguments = camp daemon --camp <dir>`, `RunAtLoad` + `KeepAlive`,
  `AbandonProcessGroup`),
  loaded via `launchctl bootstrap gui/$UID`; Linux → a systemd user unit
  `campd-<camp-id>.service` (`ExecStart=camp daemon --camp <dir>`,
  `KillMode=process`, `Restart=always`), `systemctl --user enable --now`.
  `<camp-id>` is a stable slug of the camp's absolute path (collision-free,
  human-readable).

  **Each unit names its stop shape, because each platform's default is to sweep
  campd's children** (decision 12; issue #119). §7's whole point is that a
  supervisor's stop is the SIGTERM shape: campd alone is signalled, appends
  `campd.stopped`, unlinks the socket and exits; the in-flight `claude -p`
  workers survive, are reparented, and `patrol::adopt` re-adopts them at the next
  start. A default is not a decision, and **both** defaults contradict that shape:

  - systemd, unnamed, is `KillMode=control-group` — SIGTERM then SIGKILL to every
    process in the unit's cgroup. The unit names **`KillMode=process`**, which
    signals the main pid only. (`mixed` does not buy this: it SIGTERMs only the
    main process, then SIGKILLs whatever is left in the cgroup once
    `TimeoutStopSec` elapses — the workers.)
  - launchd, unnamed, kills every process sharing the job's process group ID when
    the job dies (`man 5 launchd.plist`, `AbandonProcessGroup`, default false) —
    and workers inherit campd's pgid, since `spawn.rs` sets no `process_group`. The
    plist names **`AbandonProcessGroup=true`**, which turns that sweep off.

  macOS was **not** already at parity, and the first version of this section said
  it was: "launchd's stop is a `bootout` of a single service target (macOS has no
  cgroup to sweep), so it already signals campd and nothing else." macOS has no
  cgroup — and sweeps the process group instead. Measured on a macOS host: without
  the key, a bootout killed the job's child; with it, the child survived. Parity is
  something camp **does** on both platforms, not something either one grants.

  **The unit must carry campd's PATH, and this is not a detail.** A supervisor
  does NOT give campd the shell's environment: launchd hands a LaunchAgent
  `PATH=/usr/bin:/bin:/usr/sbin:/sbin`, and a systemd user service gets
  `/usr/local/bin:/usr/bin:/bin:…`. Neither contains `~/.local/bin`, which is
  where Claude Code installs `claude` — the process campd spawns to do all of
  the work. This was missed, and it shipped: campd came up healthy, served its
  socket, accepted beads, and failed every single dispatch with `spawn failed:
  spawning claude: No such file or directory`. Before supervision the CLI
  started campd, so campd inherited the shell that ran the verb and the question
  never arose; **removing the auto-start (§4.3) removed that inheritance, and
  nothing replaced it.** So `install` captures the PATH of the shell that runs
  it — the one environment where the operator's tools demonstrably resolve —
  writes it into the unit (`EnvironmentVariables`/`PATH`, `Environment=`), prints
  it, and warns when the configured worker command is not on it.

  It is a **snapshot**, and a snapshot goes stale — a version manager retires a
  bin directory and the camp quietly returns to the original bug. So checking
  once, at install, is a one-shot net under a permanent hazard: **`status` reads
  the PATH back out of the unit and re-asks the question every time it runs.** A
  unit carrying no PATH at all — every unit installed before this existed — is
  reported as the fault it is rather than as `loaded=true running=true`, which is
  what the installed base would otherwise keep seeing while dispatching nothing.
  The re-capture command is `camp service uninstall && camp service install`:
  `install` alone refuses to clobber an existing unit, so any message that tells
  an operator to "re-run `camp service install`" is handing them an error at the
  moment they need a fix.
- **`uninstall`** — stop + unload + remove the unit.
- **`status`** — the unit's load/run state (wraps `launchctl print` /
  `systemctl --user show`), plus the campd liveness answer (a status
  request on the socket).
- **`restart`** — cycle the daemon (post-upgrade): `launchctl kickstart -k` /
  `systemctl --user restart`.
- **`list`** — every camp with a managed unit and its state. Enumerated from
  the installed units (label prefix `com.gascamp.campd.` / `campd-*.service`)
  — no separate registry file (units are the source of truth, matching the
  no-status-files principle).
- **`stop`** — stop the supervised campd, leaving the unit INSTALLED
  (`launchctl bootout` / `systemctl --user stop`). This is what `camp stop`
  refuses in favor of on a supervised camp (decision 10). **In-flight workers
  survive it on both platforms** and are re-adopted at the next start — but only
  because each unit says so explicitly (decision 12): `KillMode=process` on
  systemd, `AbandonProcessGroup=true` on launchd. Neither default does this.
- **`start`** — start a stopped but still-installed unit (`launchctl bootstrap` /
  `systemctl --user start`).

Unit-file *generation* is pure (path in → plist/unit text out) and unit-tested;
the `launchctl`/`systemctl` calls are thin wrappers behind a seam so the
generation is testable without a live service manager.

## 6. `camp init` — environment-aware

1. Create the camp (today's behavior).
2. Detect a usable host service manager: macOS → launchd (always present for a
   GUI/user session); Linux → systemd `--user` reachable (a live user D-Bus /
   `$XDG_RUNTIME_DIR`, `systemctl --user` responds).
3. **Present** → `camp service install` + start (default). **Absent** → skip
   and print, on stderr, a visible hand-off: *"no host service manager
   detected (container/CI?) — run `camp daemon` under your supervisor (e.g. the
   container runtime)."* This is visible degradation of a convenience, not a
   silenced error (a container is not a failure) — consistent with the
   fail-fast / nothing-hidden rules.
4. Flags: `--service` forces install (hard error if no manager);
   `--no-service` skips even on a desktop.

## 7. SIGTERM handling + the container reference

**SIGTERM/SIGINT → graceful shutdown** (the one campd core change). Register
via the existing `signal_hook` self-pipe pattern (the SIGCHLD precedent in
`daemon/mod.rs`): a signal wakes the event loop, which performs the same
graceful path as `Request::Stop` (append `campd.stopped`, drop the socket,
exit 0). Crash-only means SIGKILL stays safe; this just makes a normal
`docker stop` / `systemctl stop` / `launchctl bootout` clean.

**Reference container setup** (shipped under `contrib/docker/`):

- `Dockerfile` — build the `camp` binary; a small entrypoint runs
  `camp init --no-service --exists-ok` then `exec camp daemon --camp <dir>`.
  (`camp init` is NOT idempotent on its own — it hard-errors on an existing
  camp, and a restarted container's camp volume always has one. Phase 4 adds
  `--exists-ok`: an existing camp is a no-op success. The flag, not a shell
  test, so one predicate owns "is there a camp here".)
- The image must contain **`git`**: campd shells out to `git rev-parse --verify
  HEAD^{commit}` on every dispatch to read the rig's base commit
  (`daemon/spawn.rs::rig_base`), and to `git worktree add` for the default
  isolation. It must also set a writable **`HOME`**: campd computes the worker
  transcript path under `$HOME/.claude` (a hard error if `HOME` is unset) and
  patrol creates that directory.
- Run under a minimal init (`tini` / `dumb-init`) as PID 1. **Required — and
  this design originally said otherwise.** It claimed SIGTERM handling plus
  SIGCHLD reaping made campd "PID-1-safe on its own". That is half right, and
  the wrong half matters. campd handles SIGTERM (so `docker stop` is graceful
  either way) and reaps the workers it *spawned* — but it reaps by `try_wait`
  on pids it holds, and it has no way to wait on a pid it never spawned (there
  is no `libc` dependency with which to `waitpid(-1)`). A worker's own
  subprocess, orphaned when the worker dies first, is reparented to PID 1; with
  campd as PID 1 it stays a zombie for the life of the container, leaking a PID
  slot per orphan. Measured 2026-07-11: campd as PID 1 accumulated a zombie per
  orphan; behind tini, zero. `docker run --init` / compose `init: true` is an
  equivalent substitute. No init at all is not.
- `compose.yaml` — `restart: unless-stopped`.
- CLI usage: `docker exec <container> camp sling "…"` (connects over the
  in-container socket); mount the camp dir to reach the socket from outside.

## 8. Reconciliation + spec amendments

Made by the implementation PR (`docs/design/2026-07-05-gas-camp-design.md`),
same PR as the code:

- **§5 (campd lifecycle) rewrite:** "auto-start on demand (the CLI spawns a
  detached campd)" → "campd is a supervised foreground process; the CLI is a
  pure client; supervision is environment-provided (launchd / systemd `--user`
  / container runtime / manual `camp daemon`); `camp init` installs a host
  unit when a manager is present." The `autostart::request_with_autostart`
  path is removed; daemon-needing verbs connect and fail loudly if campd is
  down. Liveness-as-answered-request and crash-only are unchanged.
- **§9 (orders):** note that always-on supervision removes the "no wake source,
  no fire" away-mode gap for the supervised case — scheduled orders fire
  because campd is kept alive; the honest limit now applies only to the
  no-service-manager fallback.
- **§12 (multi-rig):** add the standalone-camp-many-rigs recommendation as the
  way to bound daemon count under always-on.
- The existing `contrib/launchd/` bare `RunAtLoad` example is superseded by
  `camp service install` (which generates a `KeepAlive` unit); fold it into the
  new `camp service` docs / reference.

**Migration blast radius (for the plan):** removing CLI auto-start touches
every verb that calls `request_with_autostart` — verified as `top` (Status),
`adopt` (Adopt), and `sling` (the dispatch poke, both Tier-0 and formula) —
plus `daemon/autostart.rs` itself and its tests. The plan must convert these
to pure-client connects with loud errors and update/replace the
autostart-based tests. Note: the post-write `poke_best_effort` (spec §7.2, the
one sanctioned ignore-the-error site) is a *separate* best-effort poke, not
the CLI-spawn path, and is unaffected.

## 9. Testing strategy (TDD, strict)

Failing test first for each:

- **SIGTERM graceful shutdown:** spawn `camp daemon` in a temp camp, send
  SIGTERM, assert it exits 0 and appends `campd.stopped` — identical outcome to
  a socket `Request::Stop`. Same for SIGINT.
- **Unit generation (pure):** `install` unit-text generators produce the
  correct launchd plist and systemd unit for a given camp path
  (`ProgramArguments`/`ExecStart` = `camp daemon --camp <dir>`, KeepAlive /
  `Restart=always`, systemd `KillMode=process` and launchd
  `AbandonProcessGroup=true` per decision 12, stable `<camp-id>` slug). No live
  service manager needed. Both stop-shape assertions pin the **value**, not the
  presence of a line: the stop semantics is the difference between `process` and
  the `control-group` default, and between `<true/>` and the `<false/>` default,
  so a test that only checked "the key is set" would pass on the bug. They also
  pin **placement** — a `KillMode` outside `[Service]` is ignored by systemd
  while reading as though the choice was made.
- **Stop shape, at runtime:** the rendered unit is all CI can check; whether the
  directive *works* is a per-platform experiment, and the two platforms differ in
  what a dev host can answer:
  - **launchd is testable on any macOS host and MUST be tested there** — bootstrap
    a throwaway LaunchAgent mirroring camp's plist whose job spawns a child the way
    `spawn.rs` does, `launchctl bootout`, assert the child is **alive** and the job
    gone (and that it is **dead** without `AbandonProcessGroup`). This is exactly the
    experiment whose absence let the false "macOS is already at parity" claim into
    this document; it takes seconds, and it is not optional when the platform is in
    reach. Tear the agent down afterwards.
  - **systemd's runtime effect is unobservable on a macOS dev host** and belongs to
    the opt-in `camp service` integration test on a real systemd box.
- **Environment detection:** the detect-service-manager function returns
  install/skip for representative environments (macOS, systemd-user present,
  none) via injected probes.
- **CLI-as-pure-client:** a daemon-needing verb with campd down fails loudly
  (names the remedy) and does **not** spawn a daemon — assert no new process,
  actionable error text.
- **`camp service` integration** (opt-in, local-only, like `make e2e`):
  install → status shows running → restart → uninstall, on the host's real
  service manager. Gated behind an env flag; not in unit CI.
- **Container smoke (opt-in):** build the reference image, run it, `docker
  exec camp sling`, confirm the bead is dispatched and `docker stop` is clean
  (SIGTERM). Gated/local, documented alongside `make e2e`.
- Gates green before push: `cargo fmt --all --check`, `cargo clippy
  --workspace --all-targets --all-features -- -D warnings`, `cargo test
  --workspace`.

## 10. Invariants respected

- **#1 idle is free:** always-on campd sleeps on OS events (no ticks); idle
  footprint stays < 20 MB / 0.0% CPU — the invariant's actual measure.
- **#3 nothing hidden:** every campd start/stop is already a ledger event; the
  `camp init` hand-off message is visible, not a silent fallback; `camp
  service list` reads live units, not status files.
- **#5 fail fast:** a down campd is a loud CLI error, never a silent respawn;
  `--service` on a manager-less host is a hard error.
- No new event types are required (SIGTERM reuses `campd.stopped`); the
  vocabulary mirror (invariant 7) is untouched.

## 11. Out of scope / follow-ups

- Ephemeral one-shot container usage (sling-and-exit) — camp is durable async.
- A network/remote socket transport for cross-host CLI → campd.
- Auto-migrating existing camps to managed units (users run `camp service
  install` on camps created before this).
- Patrol stall-timer hot-reload is included as a small extension (§4.8) but may
  be split to a follow-up if it complicates the config-reload path.

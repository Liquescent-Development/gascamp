# camp in a container — the reference setup

The container runtime is a **supervisor**, exactly like launchd or systemd
`--user` (v1 spec §5): `camp daemon` is the container's main process, the
runtime restarts it if it dies, and `docker stop` is a SIGTERM campd answers by
appending `campd.stopped`, unlinking its socket, and exiting 0. There is no
service manager inside the container and camp does not install one — the
entrypoint runs `camp init --no-service --exists-ok`, then `exec`s campd.

## Run it

```sh
docker compose -f contrib/docker/compose.yaml up -d --build
docker compose -f contrib/docker/compose.yaml logs -f campd     # "campd listening on /camp/campd.sock"
```

Or without compose:

```sh
docker build -f contrib/docker/Dockerfile -t gascamp:latest .   # from the repo root
docker volume create camp
docker run -d --name gascamp --restart unless-stopped -v camp:/camp gascamp:latest
```

## Drive it

The camp CLI is a **pure socket client**: it talks to campd over
`<camp>/campd.sock` and never starts it. Inside the container the socket is
right there, so `docker exec` is the way in (the image sets `CAMP_DIR=/camp`, so
no `--camp` flag is needed):

```sh
docker exec gascamp camp sling "fix the flaky auth test"
docker exec gascamp camp top
docker exec gascamp camp ls --ready
docker exec gascamp camp events --json | tail -5
```

## Stop it

```sh
docker stop gascamp        # SIGTERM -> graceful: campd.stopped in the ledger, exit 0
```

`camp stop` inside the container also works (this camp is unsupervised as far as
camp is concerned — the supervisor is outside it), but the runtime will restart
campd if you asked for `restart: unless-stopped`. **Stop the container, not the
daemon.**

## Make it useful: a rig and a worker

The image ships `camp` and `git` and nothing else. A camp that dispatches real
work needs three things in `/camp/camp.toml`, all of which you can put on the
volume before the first start (or edit live — campd hot-reloads `camp.toml`):

```toml
[camp]
name = "dev"

[[rigs]]
name = "gascity"
path = "/rigs/gascity"       # mount your repo here
prefix = "gc"

[dispatch]
command = "claude"           # the worker executable (the default)
default_agent = "dev"
```

"Mount your repo here" is literal, and it is the one line `compose.yaml` cannot
write for you: nothing is mounted at `/rigs/gascity` by default, so uncomment
the rig volume in `compose.yaml` (`- /path/to/repo:/rigs/gascity`) and keep the
container path identical to the `[[rigs]] path` above. Mount it **writable** —
the default isolation is `worktree`, and `git worktree add` writes the new
branch ref into the rig's `.git`, so a `:ro` mount fails every dispatch. If the
rig path is missing entirely, campd says so in the ledger (`dispatch.failed`)
rather than crash-looping — a loud no, not a silent one.

...plus an agent definition in `/camp/agents/dev.md`. `command = "claude"` means
the image needs the Claude Code CLI and its credentials: build an image `FROM
gascamp:latest` that installs it, and mount the credentials in. The reference
image deliberately stops short of that — it is the supervision reference, not a
worker-provisioning one — and campd will tell you the truth if the worker is
missing: a `dispatch.failed` event whose `reason` names the failure, in the
ledger, where every camp failure goes.

## What does NOT work — read this before you mount the camp dir on the host

- **Reaching the socket from the host is a Linux-only trick.** Bind-mount the
  camp dir (`-v /srv/camp:/camp`) and, on a **native Linux** host, a host-side
  `camp --camp /srv/camp top` can connect to `/srv/camp/campd.sock` — same
  kernel, same socket. On **Docker Desktop (macOS/Windows)** it cannot: the
  container's filesystem is shared into a VM, a unix socket created in there is
  not a socket the host can connect to, and SQLite's WAL locking is not safe
  across that share. Even on Linux, the host `camp` must be a build whose ledger
  `schema_version` matches the container's — opening a camp with a different
  schema version is a hard error, never an auto-upgrade (v1 spec §7.1). Use
  `docker exec` — it is the supported path everywhere, and it is by definition
  the same binary that wrote the ledger.
- **The container user owns the camp.** The image runs as uid 10001 (`camp`). A
  *named* volume inherits that ownership automatically. A *bind* mount does not
  — the host directory's ownership wins, so either `chown 10001` it or run with
  `--user "$(id -u):$(id -g)"` (and make `$HOME` writable for that uid, because
  campd puts worker transcripts under it).
- **Cross-host is out of scope.** campd serves a unix-domain socket, full stop;
  there is no network transport, so a CLI on another machine cannot reach it
  (campd-service design §11).
- **This is not a one-shot runner.** `docker run gascamp camp sling "…"` and exit
  is not a supported shape: camp is durable async work, and the bead only moves
  while campd runs (design §11). Keep the container up.
- **Existing camps are not auto-migrated.** Pointing this image at a camp dir
  that already has a host service unit does not un-manage it; `camp service
  uninstall` on the host is an explicit act (design §11).

## Why tini

campd reaps its own worker children (a SIGCHLD self-pipe) and handles SIGTERM,
so it is PID-1-safe on its own. `tini` is belt and braces — it also means an
adopted orphan from a worker's own subprocess tree can never accumulate. If you
would rather not have it, `docker run --init` (or compose's `init: true`) does
the same job with the runtime's own init.

## Why the build context is the repo root

The Dockerfile compiles `camp` from source, so it needs the workspace:
`Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, `crates/` — **and `plugin/`**,
because the binary embeds `plugin/skills/worker/SKILL.md` at compile time
(`include_str!` in `daemon/spawn.rs`). That is why the repo-root `.dockerignore`
excludes `docs/` and `packs/` but deliberately does **not** exclude `plugin/`.

## The smoke test

`make container-smoke` builds this image, runs it, slings a bead over the
in-container socket, asserts the bead is dispatched and closed by a worker
inside the container, and asserts `docker stop` is fast, graceful, and exit 0.
It is opt-in and local-only (`CAMP_CONTAINER_E2E=1` + `#[ignore]`) — CI never
builds or runs Docker.

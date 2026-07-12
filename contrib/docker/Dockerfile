# Gas Camp — the reference container (v1 spec §5; campd-service design §3, §7).
#
# The container runtime IS the supervisor. campd is the container's main
# process: it gets `docker stop`'s SIGTERM directly and shuts down gracefully
# (appends campd.stopped, unlinks the socket, exits 0), and compose's
# `restart: unless-stopped` is this environment's KeepAlive. There is no
# service manager inside the container and there is nothing to install.
#
# Build from the REPO ROOT — the build context is the whole workspace:
#   docker build -f contrib/docker/Dockerfile -t gascamp:latest .
# See contrib/docker/README.md.

# ---- build ----------------------------------------------------------------
FROM rust:1-slim-bookworm AS build

# rusqlite is `bundled` (crates/camp-core/Cargo.toml): SQLite is compiled from
# C, so the builder needs a C toolchain. Named explicitly rather than assumed
# from the base image.
RUN apt-get update \
 && apt-get install -y --no-install-recommends gcc libc6-dev \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /src
# rust-toolchain.toml comes along on purpose: the image builds camp with the
# toolchain the repo pins, not whatever the base image happened to ship.
COPY rust-toolchain.toml Cargo.toml Cargo.lock ./
COPY crates ./crates
# plugin/ is a BUILD input, not just shipped content: daemon/spawn.rs embeds
# plugin/skills/worker/SKILL.md into the binary with include_str!, so the build
# cannot see the workspace without it. (Keep it out of .dockerignore too.)
COPY plugin ./plugin
RUN cargo build --release --locked -p camp

# ---- runtime --------------------------------------------------------------
FROM debian:bookworm-slim

# git             campd shells out to `git rev-parse --verify HEAD^{commit}` on
#                 EVERY dispatch to read the rig's base commit
#                 (daemon/spawn.rs::rig_base), and to `git worktree add` for the
#                 default isolation (spec §12). No git, no dispatch.
# tini            a real init as PID 1. Belt and braces: campd reaps its own
#                 workers (SIGCHLD self-pipe) and handles SIGTERM (Phase 1), so
#                 it is PID-1-safe on its own — tini just means you never have
#                 to think about it. `docker run --init` / compose `init: true`
#                 is the same thing by another route.
# ca-certificates for the worker you wire into [dispatch].command (a real
#                 Claude Code worker talks TLS). camp itself opens no sockets
#                 but the unix one.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates git tini \
 && rm -rf /var/lib/apt/lists/*

COPY --from=build /src/target/release/camp /usr/local/bin/camp
# The same `campd` argv0 alias `make install` ships: main.rs keys the daemon
# path off a "campd" file stem, so `ps` shows what is actually running.
RUN ln -sf camp /usr/local/bin/campd

COPY contrib/docker/entrypoint.sh /usr/local/bin/camp-entrypoint
RUN chmod 0755 /usr/local/bin/camp-entrypoint

# A non-root camp user owns /camp. A NAMED VOLUME mounted there inherits this
# directory's ownership when Docker first initializes it, so campd can write the
# ledger, the socket, runs/ and sessions/ with no chown dance. (A BIND mount
# does not: the host's ownership wins — see README.md.)
RUN useradd --create-home --uid 10001 camp \
 && mkdir -p /camp \
 && chown camp:camp /camp
USER camp

# HOME must exist and be writable: campd computes the worker transcript path
# under $HOME/.claude (daemon/spawn.rs::claude_config_root — a HARD ERROR when
# HOME is unset) and health patrol creates that directory (daemon/patrol.rs).
# CAMP_DIR is why `docker exec <c> camp sling "…"` needs no --camp flag.
ENV HOME=/home/camp \
    CAMP_DIR=/camp

VOLUME ["/camp"]

# tini forwards SIGTERM to its only child — the campd the entrypoint exec'd —
# and exits with campd's status, so `docker stop` yields exit code 0.
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/camp-entrypoint"]

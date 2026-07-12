# Gas Camp — developer targets.
#
# The perf/volume suite is LOCAL-ONLY by decision (2026-07-05): it is never
# run in CI. It asserts the spec §14 cost-budget numbers exactly, in
# --release, single-threaded (so timing/CPU measurements are isolated), and
# prints each measured value via --nocapture for the PR record.

# Install prefix — override with `make install PREFIX=/usr/local` (or any
# writable dir). The binary lands in $(PREFIX)/bin; add that to your PATH.
PREFIX ?= $(HOME)/.local
BINDIR := $(PREFIX)/bin

.PHONY: install uninstall perf e2e service-e2e container-smoke

# Build the release binary and install `camp` into $(BINDIR), plus the
# `campd` symlink that argv0 dispatch uses to run the daemon (main.rs keys
# the daemon path off a "campd" file stem). The symlink is relative so the
# pair relocates cleanly. Re-running is idempotent.
install:
	cargo build --release
	mkdir -p "$(BINDIR)"
	install -m 0755 target/release/camp "$(BINDIR)/camp"
	ln -sf camp "$(BINDIR)/campd"
	@echo "installed camp + campd to $(BINDIR)"
	@echo "ensure $(BINDIR) is on your PATH (e.g. export PATH=\"$(BINDIR):\$$PATH\")"

# Remove the binary and the campd symlink from $(BINDIR).
uninstall:
	rm -f "$(BINDIR)/camp" "$(BINDIR)/campd"
	@echo "removed camp + campd from $(BINDIR)"

perf:
	cargo test --release -p camp-core --test perf_volume -- --ignored --nocapture --test-threads=1
	cargo test --release -p camp --test perf_daemon -- --ignored --nocapture --test-threads=1

# Opt-in real-`claude` end-to-end suite (spec §16). LOCAL-ONLY and
# OPERATOR-GATED: CI never runs it (#[ignore]d); the run spends real Anthropic
# API money via real claude workers. Requires an authenticated `claude`,
# `python3`, and `git` on PATH. --release keeps the idle-CPU/latency numbers
# comparable to `make perf`; single-threaded isolates the measurements.
e2e:
	CAMP_E2E=1 cargo test --release -p camp --test e2e -- --ignored --nocapture --test-threads=1

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

# Opt-in reference-container smoke (design §9). LOCAL-ONLY and never in CI: the
# test is #[ignore]d AND gated on CAMP_CONTAINER_E2E=1. It builds
# contrib/docker/Dockerfile, runs the image, slings a bead over the
# in-container socket, and asserts `docker stop` is a graceful SIGTERM (exit 0,
# campd.stopped in the ledger). Requires `docker` on PATH.
container-smoke:
	CAMP_CONTAINER_E2E=1 cargo test -p camp --test container_smoke -- --ignored --nocapture --test-threads=1

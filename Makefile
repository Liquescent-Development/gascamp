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

.PHONY: install uninstall perf e2e

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

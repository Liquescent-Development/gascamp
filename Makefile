# Gas Camp — developer targets.
#
# The perf/volume suite is LOCAL-ONLY by decision (2026-07-05): it is never
# run in CI. It asserts the spec §14 cost-budget numbers exactly, in
# --release, single-threaded (so timing/CPU measurements are isolated), and
# prints each measured value via --nocapture for the PR record.
.PHONY: perf e2e

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

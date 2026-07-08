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

# Opt-in real-`claude` end-to-end suite (spec §16). Delivered by Phase 15
# (phase-15-e2e); this placeholder fails loudly until then rather than
# silently passing.
e2e:
	@echo "make e2e: the real-claude e2e suite is delivered by Phase 15 (not yet on this branch)" >&2
	@exit 1

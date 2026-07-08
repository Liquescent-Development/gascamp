#!/bin/sh
# guarded-change verification (spec §8.2): the toy project's own test suite
# must be green. Fail fast — any nonzero test result fails the check.
set -eu
exec python3 -m unittest discover -s . -p 'test_*.py' -v

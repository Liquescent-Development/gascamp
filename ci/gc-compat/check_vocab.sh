#!/usr/bin/env bash
# Spec §15.2 contract 2 (vocabulary mirror) as a CI check — Phase 6.
#
# Usage: check_vocab.sh <gascity-src-dir> <gascamp-root>
#
# Asserts, against the gascity checkout at the pinned ref:
#   0. One-ref invariant: ci/gc-compat/GASCITY_REF equals the vocab pin's
#      _provenance.gascity_ref, and the checkout's HEAD equals both.
#   a. Every name in gc-vocab.json's gc lists (events, outcome,
#      final_disposition, on_exhausted) appears as an assigned string
#      constant in internal/events/events.go or internal/beadmeta/values.go.
#   b. No CAMP_SPECIFIC_EVENTS name (crates/camp-core/src/vocab.rs) appears
#      there — camp-specific names are additive, never redefinitions.
#
# Exit 0 = all assertions hold. Exit 1 = an assertion failed (drift).
# Exit 2 = usage error / missing input / empty extraction (a check that
# cannot see its inputs must fail loudly, never pass vacuously).
set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: check_vocab.sh <gascity-src-dir> <gascamp-root>" >&2
  exit 2
fi
gc_src="$1"
camp_root="$2"

ref_file="$camp_root/ci/gc-compat/GASCITY_REF"
pin_file="$camp_root/crates/camp-core/tests/fixtures/gc-vocab.json"
vocab_rs="$camp_root/crates/camp-core/src/vocab.rs"
events_go="$gc_src/internal/events/events.go"
values_go="$gc_src/internal/beadmeta/values.go"

for f in "$ref_file" "$pin_file" "$vocab_rs" "$events_go" "$values_go"; do
  if [ ! -f "$f" ]; then
    echo "check_vocab: missing input file: $f" >&2
    exit 2
  fi
done

pinned_ref="$(cat "$ref_file")"
provenance_ref="$(jq -r '._provenance.gascity_ref' "$pin_file")"
if [ "$pinned_ref" != "$provenance_ref" ]; then
  echo "check_vocab: GASCITY_REF ($pinned_ref) != gc-vocab.json provenance ($provenance_ref) — the two must reference one ref" >&2
  exit 1
fi

checkout_head="$(git -C "$gc_src" rev-parse HEAD)"
if [ "$checkout_head" != "$pinned_ref" ]; then
  echo "check_vocab: gascity checkout HEAD ($checkout_head) != GASCITY_REF ($pinned_ref)" >&2
  exit 1
fi

# Extract assigned string literals (Name = "value") from gc source.
# Matching '= "..."' skips names merely mentioned in comments.
gc_names="$(grep -hoE '= *"[^"]+"' "$events_go" "$values_go" | sed -E 's/^= *"([^"]+)"$/\1/' | sort -u || true)"
if [ -z "$gc_names" ]; then
  echo "check_vocab: extracted zero string constants from gc source — extraction is broken" >&2
  exit 2
fi

pin_names="$(jq -r '.events[], .outcome[], .work_outcome[], .final_disposition[], .on_exhausted[]' "$pin_file" | sort -u)"
if [ -z "$pin_names" ]; then
  echo "check_vocab: gc-vocab.json gc lists are empty — pin is broken" >&2
  exit 2
fi

camp_names="$(sed -n '/CAMP_SPECIFIC_EVENTS/,/];/p' "$vocab_rs" | grep -oE '"[^"]+"' | tr -d '"' | sort -u || true)"
if [ -z "$camp_names" ]; then
  echo "check_vocab: extracted zero CAMP_SPECIFIC_EVENTS names from vocab.rs — extraction is broken" >&2
  exit 2
fi

missing="$(comm -23 <(printf '%s\n' "$pin_names") <(printf '%s\n' "$gc_names"))"
if [ -n "$missing" ]; then
  echo "check_vocab: pinned gc names missing from gascity source at $pinned_ref:" >&2
  printf '%s\n' "$missing" >&2
  exit 1
fi

collisions="$(comm -12 <(printf '%s\n' "$camp_names") <(printf '%s\n' "$gc_names"))"
if [ -n "$collisions" ]; then
  echo "check_vocab: camp-specific event names found in gc source (must be additive, never redefinitions):" >&2
  printf '%s\n' "$collisions" >&2
  exit 1
fi

pin_count="$(printf '%s\n' "$pin_names" | wc -l | tr -d ' ')"
camp_count="$(printf '%s\n' "$camp_names" | wc -l | tr -d ' ')"
echo "check_vocab: OK — $pin_count pinned names present in gc source; $camp_count camp-specific names absent (ref $pinned_ref)"

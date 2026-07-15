#!/usr/bin/env python3
"""usage: mail_prime_corpus.py <corpus-checkout>

compat-4 drift guard (MEASURED at GCPACKS_REF; A7/A8), over the V1-SERVED packs.
Two invariants the mail/prime shim relies on:

  1. Every concrete `gc mail send` addresses `human`, AND at least
     MIN_HUMAN_SENDS such sends are actually examined — so a run that scanned
     the WRONG roots (and saw zero sends) is a HARD FAILURE, not a silent
     "OK". This is the C4-B1 anti-vacuity floor.
  2. Every `gc prime` occurrence is prohibition prose ("Do not run gc prime");
     a bare invocation means prime reached an executed v1 path → re-measure
     Task 6's serve-shape.

V1-served roots (A8) = the packs camp v1 imports + transitive `gascity` content.
Scanning `gascity` covers `gascity/assets` (the 6 sends) AND `gascity/roles`.
`gastown` (v2 — 41 legitimately non-human sends) and `oversight-rig` are
DELIBERATELY NOT scanned. Exits nonzero on any violation or a below-floor
count, naming the file.
"""

import pathlib
import re
import sys

V1_ROOTS = ["bmad", "gstack", "compound-engineering", "superpowers", "gascity"]
MIN_HUMAN_SENDS = 8  # measured at GCPACKS_REF (A8): 6 gascity/assets + 2 superpowers/assets
# The `(\S+)` recipient capture is greedy: it grabs the whole next whitespace-
# delimited token. At GCPACKS_REF every real recipient is a clean token
# (`human`, `mayor`, `$WITNESS_TARGET`, …), so this is exact here. If a future
# corpus writes prose like "gc mail send human, then …", the capture would be
# `human,` and MISS the `== "human"` match — flagging it as non-human. That is a
# SAFE failure (a human re-measures A8), not a false pass; leave it greedy.
SEND_RE = re.compile(r"gc\s+mail\s+send\s+(?:--to\s+)?(\S+)")
PROHIBITION = re.compile(r"do not|don't|never", re.IGNORECASE)


def v1_files(root: pathlib.Path):
    for pack in V1_ROOTS:
        base = root / pack
        if not base.is_dir():
            continue
        for p in base.rglob("*"):
            if p.is_file() and p.suffix in (".md", ".toml", ".tmpl"):
                yield p


def main() -> int:
    root = pathlib.Path(sys.argv[1])
    problems, human_sends, prime_prohibitions, files = [], 0, 0, 0
    for p in v1_files(root):
        files += 1
        text = p.read_text(encoding="utf-8", errors="replace")
        for m in SEND_RE.finditer(text):
            rcpt = m.group(1)
            # A flag/placeholder/variable token is not a concrete recipient.
            if rcpt.startswith(("-", "{{", "...", "<", "`", '"', "'", "$")):
                continue
            if rcpt == "human":
                human_sends += 1
            else:
                problems.append(
                    f"{p}: `gc mail send {rcpt}` is not `human` (v1 is send-human only)"
                )
        for ln in text.splitlines():
            if "gc prime" in ln:
                if PROHIBITION.search(ln):
                    prime_prohibitions += 1
                else:
                    problems.append(
                        f"{p}: `gc prime` invoked, not prohibited ({ln.strip()!r}) — "
                        "prime reached a v1 path; re-measure Task 6"
                    )
    # THE anti-vacuity assertion (C4-B1): a guard that examined too few sends
    # scanned the wrong roots or the corpus moved. Either way, do NOT pass.
    if human_sends < MIN_HUMAN_SENDS:
        problems.append(
            f"VACUOUS: examined only {human_sends} `send human` calls across {files} v1 files "
            f"(floor {MIN_HUMAN_SENDS}); the guard scanned the wrong roots or the corpus moved — "
            "re-measure (A8) before touching this."
        )
    if problems:
        print("compat-4 corpus drift:", *problems, sep="\n  ")
        return 1
    print(
        f"compat-4 corpus guard OK: {human_sends} `send human` (floor {MIN_HUMAN_SENDS}), "
        f"0 non-human; {prime_prohibitions} `gc prime` all prohibition-prose, across {files} v1 files"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())

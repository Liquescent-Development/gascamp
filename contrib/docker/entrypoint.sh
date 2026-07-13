#!/bin/sh
# The camp container's entrypoint (v1 spec §5; campd-service design §7).
#
# Two lines of work, and the second one is the important one:
#
#   1. Make sure the camp exists. `--exists-ok` is what makes a RESTART work:
#      the camp dir is a volume, so on the second start the camp is already
#      there, and a bare `camp init` would exit 1 and crash-loop the container.
#      `--no-service` because there is no host service manager in here and none
#      is wanted — the container runtime is the supervisor.
#   2. BECOME campd. `exec` matters: campd must be tini's direct child, so that
#      `docker stop`'s SIGTERM lands on campd itself (graceful shutdown, spec
#      §5) instead of on a shell that would ignore it and get SIGKILLed.
#
# Anything you need before campd starts (a rig checkout, credentials for the
# worker in [dispatch].command) belongs in front of the exec, or in an image
# built FROM this one.
set -eu

: "${CAMP_DIR:?CAMP_DIR must name the camp directory (the image sets it to /camp)}"
# Optional: a pack source to import as the starter (a local path or URL). When
# set, the camp is initialized with it; on a restart (--exists-ok) the import
# is re-materialized idempotently. When unset and not a TTY, init hands off and
# the camp starts empty (the operator imports a pack themselves).
: "${CAMP_PACK:=}"

if [ -n "$CAMP_PACK" ]; then
  camp init --camp "$CAMP_DIR" --no-service --exists-ok --import "$CAMP_PACK"
  # Re-materialize every locked import on each start (idempotent; never re-resolves a ref).
  camp import install --camp "$CAMP_DIR"
else
  camp init --camp "$CAMP_DIR" --no-service --exists-ok
fi

exec camp daemon --camp "$CAMP_DIR"

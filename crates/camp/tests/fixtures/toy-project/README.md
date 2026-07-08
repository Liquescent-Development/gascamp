# toy-project

A tiny, dependency-free Python CLI used as a gas-camp e2e fixture. A worker is
slung against a throwaway copy of this directory and asked to extend it.

## Run

    ./toy ls                 # list the items
    python3 -m unittest -v   # run the test suite
    scripts/verify.sh        # what the guarded-change formula's check runs

The e2e harness copies this directory into a temp rig, `git init`s it, and
points a camp rig at the copy — the checked-in fixture stays pristine.

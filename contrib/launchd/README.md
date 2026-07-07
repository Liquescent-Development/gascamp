# campd at login (optional launchd agent)

Orders (spec §9) fire only while `campd` runs. By default campd starts on
demand — the first `camp` verb after boot brings it up — so a freshly
rebooted, untouched machine fires nothing. If you want orders firing from
login without running a `camp` command first, install this agent:

    sed -e "s|/usr/local/bin/camp|$(command -v camp)|" \
        -e "s|/Users/YOU/camps/dev/.camp|$HOME/camps/dev/.camp|" \
        contrib/launchd/com.gascamp.campd.plist.example \
        > ~/Library/LaunchAgents/com.gascamp.campd.plist \
    && launchctl load ~/Library/LaunchAgents/com.gascamp.campd.plist

Adjust the camp path; one plist per camp. `launchctl unload …` removes it.
It is an example, never auto-installed — visible automation only.

## The honest away-mode limits (spec §9)

- An order fires, campd cooks and dispatches, everything lands in the
  ledger; you come back and the ledger tells the story. Away-mode is the
  same code path as attended use — there is no separate mode.
- With the default on-demand daemon, orders fire only between the first
  `camp` use and `camp stop`/reboot. This agent closes the from-login gap.
- A powered-off or sleeping laptop fires nothing until wake. On wake,
  fires missed within an order's `catch_up_window` (default `"2h"`; `"0"`
  disables) fire once, flagged `catch_up: true`; older ones are skipped.
- campd never guards against self-triggering orders: an order matching an
  event its own formula run produces recurses, visibly, in the ledger.
  That power is yours, like a `* * * * *` cron on an expensive formula.

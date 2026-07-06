# Gas Camp

**Status: design phase.** Nothing here runs yet.

Gas Camp is to [Gas City](https://github.com/gastownhall/gascity) what k3s is
to k8s: the same six orchestration primitives, sized down to one small Rust
binary. A single SQLite ledger instead of Dolt, Claude Code as the agent
runtime, zero CPU when idle, and nothing hidden — every agent visible,
tailable, and conversable. Camp for lunch, city for fleets, and a documented
migration path from one to the other.

Start with the design document: [`docs/design/2026-07-05-gas-camp-design.md`](docs/design/2026-07-05-gas-camp-design.md).

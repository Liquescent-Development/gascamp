# Phase 8 — Dispatch and Workers: Implementation Plan

> **Plan approved by Opus 4.8 plan review, 2026-07-07 (automated plan gate per operator directive).** Execution rulings: decisions F/C/B accepted; decision G's sessions/ capture adopted; Task 12's spec §7.1 commit HELD pending operator sequencing — execute all other tasks.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Spec §8.1/§8.4/§12 made real: pack agent resolution, headless-but-present worker spawning with registry-at-birth, SIGCHLD reaping, worktree isolation, `camp sling` end to end, and the fake agent that makes all of it CI-testable — the Tier-0 path complete and evented with real-`claude` spawn arguments matching fixture facts F1–F7 exactly.

**Architecture:** `camp-core` gains pure logic only (agent-definition parsing with last-wins pack layering, the dispatchable-beads query, session-name allocation, four new event types with fold arms). The `camp` binary gains the dispatcher: a `Dispatcher` owned by the daemon that, on every wake (startup catch-up, socket poke, SIGCHLD), converges the ledger's dispatchable set onto live worker children up to the concurrency cap — registry row committed **before** exec (F1), exit codes mapped to `session.stopped`/`session.crashed` (F4) via a signal-hook self-pipe into the existing mio poll loop. No ticks, no timers added; the poll timeout stays `None`.

**Tech Stack:** Existing workspace (Rust edition 2024, rusqlite, mio, serde/toml, clap, anyhow/thiserror). New: `signal-hook` (SIGCHLD self-pipe, no unsafe in our code), `uuid` v4 (pre-assigned `--session-id`, F1), and a YAML frontmatter reader in camp-core for Claude Code agent files (see decision A).

**Authority:** `docs/design/2026-07-05-gas-camp-design.md` (spec, §4 settled); master plan `docs/superpowers/plans/2026-07-05-gas-camp-v1-implementation.md` § "Phase 8 — Dispatch and Workers" (binding contract); `docs/design/2026-07-06-assumption-findings.md` fixture facts F1–F7 (BINDING for spawn design).

## Global Constraints

Copied from AGENTS.md, the master plan, and the phase kickoff. Every task's requirements implicitly include this section.

- **Branch `phase-8-dispatch-workers`; never commit to main.** One reviewable PR. No co-author lines, no self-mention in commits. Conventional-commit style.
- **Gates before push:** `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace`. Work is complete only when pushed and CI is green (`gh pr checks --watch`).
- **TDD strictly:** failing test first, watch it fail, implement, watch it pass. Run every new/changed test.
- **Idle is free (invariant 1):** no ticks, no polling loops in camp code. The SIGCHLD self-pipe and socket are OS events; `poll_timeout()` stays `None` (Phase 10 owns extending it). Test-harness waits may poll — camp itself never.
- **A write is one transaction:** every mutation goes through `Ledger::append`/`append_batch`; new events use `#[serde(deny_unknown_fields)]` payload structs; the one-transaction event+state property and the refold property test stay green.
- **Vocabulary mirror (invariant 7):** `bead.worktree.reaped` is gc-mirrored (verified present in `gc-vocab.json` events list); `worker.milestone`, `worktree.kept`, `dispatch.failed` are camp-specific (verified absent from gc's list — gc has `worker.operation`, not `worker.milestone`). `campd.autostarted` **already exists** (Phase 7) — verify, do not re-add. The vocab-pin partition tests must pass.
- **Fail fast, no fallbacks:** no panics in library code (`unwrap_used`/`expect_used`/`panic` denied, `#![forbid(unsafe_code)]`); every campd error surfaces to a caller or lands in the ledger as an event.
- **Zero role names in machinery code** (invariant 4). Agent names live in config and pack content only.
- **Respect merged interfaces — extend, don't rework:** `EventProcessor`/`ReadinessProcessor`/`catch_up` (Phase 7), `cook` (Phase 5), readiness rule (Phase 3 / plan decision 6). `crates/camp/src/daemon/event_loop.rs` is the highest-overlap file with in-flight phase-10-orders: keep edits there additive and minimal (SIGCHLD wiring + dispatch hooks only); flag the lead before anything beyond that.
- **Shared small-conflict files** (`main.rs`, `event.rs`, `vocab.rs`, `fold.rs`, `config.rs`, `Cargo.toml`s, `Cargo.lock`): additive edits only. On lead's notice of a sibling merge: rebase onto current main immediately, resolve, re-run all gates; never push/update the PR from a stale base.
- **Spec and code never diverge silently:** this phase adds one line to spec §7.1 (the `sessions/` capture directory, decision G) in the same PR.

## Plan-Time Decision Log

Decisions made while writing this plan, so execution does not re-derive them. Letters continue camp's convention of naming in-code references.

- **A. Agent-file frontmatter parsing.** Agent definitions are Claude Code files verbatim (spec §11 "zero invented formats"), so camp parses Claude Code's format and key spellings and **tolerates unknown frontmatter keys** (Claude Code owns that format and adds keys over time; rejecting them would break valid packs — the one deliberate exception to camp's deny-unknown habit, which applies to camp-owned formats). Keys camp reads are type-checked strictly; a wrong type or malformed frontmatter is a hard error naming the file and key. *(Key spellings verified against the official sub-agents documentation and 12 installed agent files; the full fact table and citations are pinned in Task 4.)*
- **B. Query-driven dispatch convergence.** The dispatcher converges from ledger truth on every wake: `dispatchable_beads()` (decision C) is re-queried and workers spawned up to the cap. An in-memory queue of `take_pending()` hints was rejected: it is crash-lossy (kill -9 forgets cap-overflow beads), misses beads slung while campd was down only accidentally-covered by cursor position, and duplicates what the state tables already answer in microseconds. `ReadinessProcessor` and its `take_pending()` are **unchanged** (Phase 7 shipped them; Phase 9 will use the hint attribution for check/retry cause chains); the drain call sites now feed the dispatcher's converge step, and reads happen only on wakes — camp has queries, no query loops (spec §7.3).
- **C. Dispatchable bead = structure, not judgment.** `status='open'` ∧ ready (plan decision 6 rule) ∧ `type='task'` (memory/mail beads are records, not work) ∧ not a run root (`run_id` set with `step_id` NULL — roots are finalized by campd in Phase 9, never worked) ∧ **no sessions row bound to the bead** (`sessions.bead`). The last clause makes dispatch exactly-once per bead and deliberately excludes respawn-after-crash: without retry budgets (Phase 9) and backoff ladders (Phase 11), auto-respawn would be an unbounded crash-spawn loop. A crashed worker's bead returns to `open` (existing fold), stays visible in `camp ls --ready`, and the Phase 9/11 machinery earns the respawn.
- **D. Routing order, resolved mechanically:** `bead.assignee` → the bead's rig's `default_agent` → `[dispatch].default_agent`. `camp sling` resolves this **at sling time**, stamps the winner into `bead.created.data.assignee`, and errors fast when no layer names an agent (message names all three fixes). campd applies the same order at dispatch time for beads without an assignee (cooked formula steps).
- **E. Registry-at-birth without pid.** `session.woke` commits BEFORE exec (F1) with session name, agent, rig, pre-assigned claude session id, computed transcript path, bead, and worktree path (when isolated) — no pid (unknowable pre-exec; the master-plan contract omits it deliberately). campd holds the `Child` in memory; pid-in-registry is a Phase 11 seam (adoption can match live workers by `--session-id` argv).
- **F. campd dispatch errors land in the ledger.** campd has no caller, so per-bead dispatch failures (unresolvable agent, unknown/missing rig path, worktree creation failure, transcript-path computation failure) append camp-specific **`dispatch.failed`** (bead set, `data.reason`) — one per bead per campd lifetime (in-memory guard; a restart retries once, crash-only). A spawn that fails **after** `session.woke` committed appends `session.crashed` with `data.reason` instead — the registry row must not dangle live. ⚠ `dispatch.failed` is a vocabulary addition beyond the kickoff's named list; it exists because invariant 5 demands campd errors land in the ledger. Flagged for lead/operator sign-off with this plan.
- **G. Worker stdout/stderr capture.** Workers spawn with stdout → `<camp>/sessions/<munged-session-name>.json` (the F2 result envelope, kept for forensics/Phase 9+) and stderr → `<camp>/sessions/<munged-session-name>.log`; munge = every non-alphanumeric byte of the session name → `-`. **No envelope parsing in Phase 8**: F4 pins that failure routing comes from the worker contract (close events), and exit-code mapping needs no envelope. Spec §7.1 layout gains one line for `sessions/` in this PR (same-change spec rule).
- **H. Worktrees.** `isolation = "worktree"` ⇒ cwd = `<camp>/worktrees/<bead-id>`, created via `git -C <rig-path> worktree add -b camp/<bead-id> <dir>` (fresh branch from the rig's HEAD; a pre-existing dir/branch is a hard `dispatch.failed` — bead ids are unique and Phase 8 never respawns). Disposition at reap: bead closed with `outcome='pass'` ⇒ `git worktree remove --force` + gc-mirrored `bead.worktree.reaped` event; anything else (fail outcome, unclosed bead, crash) ⇒ keep + camp-specific `worktree.kept` event with the reason. A failed removal also keeps, with the git error as the reason — never silent. The branch is left standing in both cases (it may hold unpushed work; deleting it would destroy forensics — sweep policy belongs to Phase 11 adoption).
- **I. SIGCHLD via signal-hook self-pipe.** `signal_hook::low_level::pipe::register(SIGCHLD, write_end)` of a `UnixStream::pair()`; the nonblocking read end joins the mio poll as **`Token(2)`, with connection tokens starting at 3** — `Token(1)` is left free because in-flight phase-10-orders reserves it for its CONFIG_WATCH notify pipe (lead coordination note, 2026-07-07; shared token layout for the reviewer: 0 = listener, 1 = config watch (Phase 10), 2 = SIGCHLD (this phase), 3+ = connections; whichever phase merges second rebases onto this layout). On wake: drain the pipe, `Child::try_wait()` every tracked child (safe std, no libc), map per F4 — exit 0 ⇒ `session.stopped`; nonzero exit or signal ⇒ `session.crashed` (data records `exit_code`/`signal`) — then worktree disposition, then catch-up + converge (frees capacity ⇒ next bead dispatches: the 11th-on-first-close obligation). `try_wait` re-returns the status after reap, so a failed append is safely retried on the next wake. Phase 10 also swaps the poke arm's `cursor::catch_up` call for its `orders::settle` fixpoint and adds positional parameters to `event_loop::run`; this phase's edits are one added converge call after that same seam plus its own positional parameters — line-local rebases in either merge order.
- **J. Worker environment.** campd exports `CAMP_DIR=<camp root>`, `CAMP_BEAD`, `CAMP_SESSION` to each worker; the task prompt repeats the bead id and session name textually for the model. `fake-agent.sh` consumes the env (plus `CAMP_BIN`, test-provided). Real workers need `camp` on PATH — a documented operator requirement, not camp machinery.
- **K. stdin = `/dev/null`** for every Phase 8 worker (F5's 3 s sniff penalty). Stream-json stdin-held workers are the Phase 11 nudge path, not Phase 8.
- **L. Prompt split per F7.** `--append-system-prompt` carries the agent definition's prompt body (omitted when empty); the `-p` task prompt carries the worker-contract instructions + bead id + session name. Pinning flags come from the agent definition and are emitted only when declared (`--model`, `--permission-mode`, `--allowedTools` as comma-joined list) — what a pack author leaves undeclared visibly inherits ambient config (F7's warning is answered by pack content, not machinery guesses).
- **M. Session names** are `<camp-name>/<agent>/<n>` (camp name from `[camp].name`); `n` = 1 + the highest existing suffix among `sessions` rows with that exact `<camp>/<agent>/` prefix (computed in Rust from an indexed `agent =` query — no LIKE-escaping traps). Allocation races don't exist (only campd allocates in v1), and the fold's duplicate-name rejection backstops.
- **N. Claude session ids** are uuid v4 from the `uuid` crate, generated by campd and passed via `--session-id` (F1).
- **O. Transcript path (F3):** `<root>/projects/<munge(worker cwd)>/<sid>.jsonl` where `<root>` = `$CLAUDE_CONFIG_DIR` if set else `$HOME/.claude` (`$HOME` unset ⇒ per-bead `dispatch.failed`, not a campd crash), munge = every non-ASCII-alphanumeric byte → `-`. Computed from the **worker's** cwd — the worktree path when isolated.
- **P-amendment (2026-07-07, PR #14 review finding 2, operator-approved):** the poke reply is an **ACK sent before the settle** — it means "campd is awake and will process this wake," not "processing finished." A settle slowed by worktree checkouts must not starve the poker's 5 s client timeout into a nonzero sling that retries and duplicates the bead; the bead's durability, not the ack, carries the Tier-0 promise. Task 9's original wiring text ("a converge failure answers the poker") is superseded accordingly; settle errors land on stderr and the next wake retries. Evidence: `slow_settle_does_not_starve_the_poke_ack`.
- **P. `camp sling` auto-starts campd** via the existing `request_with_autostart` poke (Tier-0 is a dispatch promise; a fire-and-forget poke to a dead daemon would break it). `create`/`claim`/`close`/`rig` keep `poke_best_effort` — they promise durability, not dispatch. Consequence, stated: any ready task bead is dispatchable once campd wakes, however it was created — `sling` is create + routing validation + ensure-daemon, not a privileged path (spec §7.3 "dispatches anything newly ready").
- **Q. Config shape.** `CampConfig` gains top-level `packs: Vec<PathBuf>` (relative paths resolve against the camp root) and a `[dispatch]` table (`max_workers` default 10, `command` default `"claude"`, `default_agent` optional); `[[rigs]]` gains optional `default_agent`. All new fields use `skip_serializing_if` so `rig add`'s TOML output is unchanged. `CampConfig` gains `#[serde(skip)] root: Option<PathBuf>` set by `load()` — this keeps the master plan's pinned `resolve_agent(cfg, name)` signature while letting relative pack paths and the local `<camp>/agents/` layer resolve. Config is loaded once at campd start (hot-reload is Phase 10's config watch).
- **R. Agent layering order** (spec §11 "last-wins with local definitions highest"): packs in `camp.toml` order (later wins), then `<camp>/agents/` as the final, highest layer. Within one directory, two files claiming the same agent name is a hard error.

## File Map

| File | Action | Responsibility |
|---|---|---|
| `crates/camp-core/src/config.rs` | modify (additive) | `packs`, `[dispatch]`, rig `default_agent`, `root` |
| `crates/camp-core/src/pack.rs` | create | AgentDef frontmatter parse, `resolve_agent` layering |
| `crates/camp-core/src/event.rs` | modify (additive) | 4 new `EventType` variants |
| `crates/camp-core/src/vocab.rs` | modify (additive) | partition the new names |
| `crates/camp-core/src/ledger/fold.rs` | modify (additive) | fold arms + extended session payloads |
| `crates/camp-core/src/readiness.rs` | modify (additive) | `dispatchable_beads` |
| `crates/camp-core/src/ledger/mod.rs` | modify (additive) | `dispatchable_beads`, `next_session_name` wrappers |
| `crates/camp-core/src/lib.rs` | modify (additive) | `pub mod pack;` |
| `crates/camp/src/cmd/event_emit.rs` | create | `camp event emit` |
| `crates/camp/src/cmd/sling.rs` | create | `camp sling` |
| `crates/camp/src/cmd/create.rs` | modify (tiny) | share `resolve_rig` |
| `crates/camp/src/cmd/rig.rs` | modify (tiny) | `RigConfig` literal gains field |
| `crates/camp/src/main.rs` | modify (additive) | `Sling`, `Event emit` subcommands |
| `crates/camp/src/daemon/spawn.rs` | create | SpawnSpec builder (F1–F7), transcript path, worktree git ops |
| `crates/camp/src/daemon/dispatch.rs` | create | `Dispatcher`: converge, reap, worktree disposition |
| `crates/camp/src/daemon/mod.rs` | modify | config load, dispatcher + SIGCHLD pipe construction, initial converge |
| `crates/camp/src/daemon/event_loop.rs` | modify (minimal) | SIGCHLD token, converge hooks |
| `crates/camp/tests/fake-agent.sh` | create | worker-contract fake agent |
| `crates/camp/tests/daemon_dispatch.rs` | create | integration suite |
| `docs/design/2026-07-05-gas-camp-design.md` | modify (one line) | §7.1 `sessions/` |
| `Cargo.toml`s / `Cargo.lock` | modify (additive) | signal-hook, uuid, YAML dep |

---

### Task 1: Config — `[dispatch]`, `packs`, rig `default_agent`

**Files:**
- Modify: `crates/camp-core/src/config.rs`
- Modify: `crates/camp/src/cmd/rig.rs` (one struct literal), `crates/camp-core/src/formula/cook.rs` tests if any `RigConfig` literal exists there (compiler will say; add `default_agent: None`)

**Interfaces:**
- Consumes: existing `CampConfig`/`CampSection`/`RigConfig` (Phase 3), `CoreError::Config`.
- Produces (later tasks rely on these exact names):

```rust
pub struct CampConfig {
    pub camp: CampSection,
    pub rigs: Vec<RigConfig>,
    pub packs: Vec<PathBuf>,            // default [], skipped when empty
    pub dispatch: DispatchConfig,       // default, skipped when default
    pub root: Option<PathBuf>,          // #[serde(skip)]; set by load()
}
pub struct RigConfig { pub name: String, pub path: PathBuf, pub prefix: String,
                       pub default_agent: Option<String> } // new, optional
pub struct DispatchConfig {
    pub max_workers: usize,             // default 10
    pub command: PathBuf,               // default "claude"
    pub default_agent: Option<String>,  // default None
}
```

- [ ] **Step 1: Write the failing tests** — append to the `tests` module in `crates/camp-core/src/config.rs`:

```rust
    #[test]
    fn dispatch_and_packs_parse_with_defaults() {
        let cfg = CampConfig::parse(
            r#"
[camp]
name = "dev"
packs = ["packs/starter", "/abs/otherpack"]

[[rigs]]
name = "gascity"
path = "/code/gascity"
prefix = "gc"
default_agent = "rigger"

[dispatch]
max_workers = 3
command = "tests/fake-agent.sh"
default_agent = "dev"
"#,
        )
        .unwrap();
        assert_eq!(
            cfg.packs,
            vec![PathBuf::from("packs/starter"), PathBuf::from("/abs/otherpack")]
        );
        assert_eq!(cfg.dispatch.max_workers, 3);
        assert_eq!(cfg.dispatch.command, PathBuf::from("tests/fake-agent.sh"));
        assert_eq!(cfg.dispatch.default_agent.as_deref(), Some("dev"));
        assert_eq!(cfg.rig("gascity").unwrap().default_agent.as_deref(), Some("rigger"));
    }

    #[test]
    fn dispatch_section_is_optional_with_spec_defaults() {
        let cfg = CampConfig::parse("[camp]\nname = \"dev\"\n").unwrap();
        assert!(cfg.packs.is_empty());
        assert_eq!(cfg.dispatch.max_workers, 10);
        assert_eq!(cfg.dispatch.command, PathBuf::from("claude"));
        assert!(cfg.dispatch.default_agent.is_none());
        assert!(cfg.root.is_none(), "parse() has no file, so no root");
    }

    #[test]
    fn unknown_dispatch_key_is_rejected() {
        let err = CampConfig::parse("[camp]\nname=\"d\"\n[dispatch]\nbogus = 1\n").unwrap_err();
        assert!(matches!(err, CoreError::Config(_)), "got {err:?}");
    }

    #[test]
    fn defaults_do_not_pollute_serialization() {
        // rig add re-serializes nothing today (it appends text), but the
        // config type must still round-trip cleanly without inventing
        // [dispatch]/packs blocks the user never wrote.
        let cfg = CampConfig::parse("[camp]\nname = \"dev\"\n").unwrap();
        let text = toml::to_string(&cfg).unwrap();
        assert!(!text.contains("dispatch"), "text was: {text}");
        assert!(!text.contains("packs"), "text was: {text}");
    }

    #[test]
    fn load_records_the_camp_root() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("camp.toml");
        std::fs::write(&path, "[camp]\nname = \"dev\"\n").unwrap();
        let cfg = CampConfig::load(&path).unwrap();
        assert_eq!(cfg.root.as_deref(), Some(dir.path()));
    }
```

Add `tempfile` usage: camp-core already has it as a dev-dependency. Add `use std::path::PathBuf;` to the test module if missing.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --package camp-core config`
Expected: FAIL — unknown fields `packs`/`dispatch`/`default_agent`, missing struct fields.

- [ ] **Step 3: Implement** — in `crates/camp-core/src/config.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampConfig {
    pub camp: CampSection,
    #[serde(default)]
    pub rigs: Vec<RigConfig>,
    /// Pack directories (spec §11). Relative paths resolve against `root`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub packs: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "DispatchConfig::is_default")]
    pub dispatch: DispatchConfig,
    /// The directory containing camp.toml — set by `load`, never serialized.
    /// Needed to resolve relative pack paths and the local agents/ layer
    /// while keeping the master plan's `resolve_agent(cfg, name)` signature.
    #[serde(skip)]
    pub root: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DispatchConfig {
    /// Concurrency cap (spec §8.3); master plan Phase 8 default.
    #[serde(default = "default_max_workers")]
    pub max_workers: usize,
    /// Worker executable. Tests point this at fake-agent.sh — visible
    /// config, not a fallback (master plan Phase 8).
    #[serde(default = "default_command")]
    pub command: PathBuf,
    /// Camp-wide sling routing default (spec §8.1); rigs may override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_agent: Option<String>,
}

fn default_max_workers() -> usize { 10 }
fn default_command() -> PathBuf { PathBuf::from("claude") }

impl Default for DispatchConfig {
    fn default() -> Self {
        DispatchConfig { max_workers: default_max_workers(), command: default_command(), default_agent: None }
    }
}

impl DispatchConfig {
    fn is_default(&self) -> bool { *self == DispatchConfig::default() }
}
```

Extend `RigConfig` with:

```rust
    /// Per-rig sling routing override (spec §8.1 "the pack's default worker
    /// for the current rig").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_agent: Option<String>,
```

Extend `load` to record the root:

```rust
    pub fn load(path: &Path) -> Result<CampConfig, CoreError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| CoreError::Config(format!("cannot read {}: {e}", path.display())))?;
        let mut cfg = CampConfig::parse(&text)?;
        cfg.root = path.parent().map(Path::to_path_buf);
        Ok(cfg)
    }
```

Fix the `RigConfig` struct literals the compiler reports (`crates/camp/src/cmd/rig.rs` `add`, plus the existing config-round-trip test in this file): add `default_agent: None`.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --package camp-core config && cargo test --workspace`
Expected: PASS (workspace run catches every `RigConfig` literal).

- [ ] **Step 5: Commit**

```bash
git add crates/camp-core/src/config.rs crates/camp/src/cmd/rig.rs
git commit -m "feat: dispatch config, pack list, and per-rig default agent in camp.toml"
```


### Task 2: Events — `worker.milestone`, `worktree.kept`, `bead.worktree.reaped`, `dispatch.failed` + extended session payloads

**Files:**
- Modify: `crates/camp-core/src/event.rs`, `crates/camp-core/src/vocab.rs`, `crates/camp-core/src/ledger/fold.rs`
- Tests live in the modified files' `#[cfg(test)]` modules and run through `crates/camp-core/tests/vocab_pin.rs` (no edit needed there — it iterates `EventType::ALL`).

**Interfaces:**
- Consumes: `EventType`/`Event`/`EventInput` (Phase 1), fold helpers `payload`/`required_bead`.
- Produces: `EventType::{WorkerMilestone, WorktreeKept, BeadWorktreeReaped, DispatchFailed}` with names `"worker.milestone"`, `"worktree.kept"`, `"bead.worktree.reaped"`, `"dispatch.failed"`. Fold contracts (all log-only — no state tables change; payloads validated, deny_unknown_fields):

| Event | Required envelope | Payload |
|---|---|---|
| `worker.milestone` | — (`bead` optional; if set, must exist) | `{text}` — non-empty |
| `worktree.kept` | `bead` (must exist) | `{path, reason}` — both non-empty |
| `bead.worktree.reaped` | `bead` (must exist) | `{path}` — non-empty |
| `dispatch.failed` | `bead` (must exist) | `{reason}` — non-empty |
| `session.woke` (extended) | as before | gains optional `worktree` |
| `session.stopped`/`session.crashed` (extended) | as before | gain optional `exit_code`, `signal`, `reason` |

- [ ] **Step 1: Write the failing tests** — append to the `tests` module in `crates/camp-core/src/ledger/fold.rs` (create the module if the file has none — it does have one via `ledger/mod.rs` tests; put these in `crates/camp-core/src/ledger/mod.rs`'s existing `#[cfg(test)]` module, which already has `append` helpers):

```rust
    // ---- Phase 8 events ------------------------------------------------

    fn seeded_bead(l: &mut Ledger, id: &str) {
        l.append(EventInput {
            kind: EventType::BeadCreated,
            rig: Some("gc".into()),
            actor: "test".into(),
            bead: Some(id.into()),
            data: serde_json::json!({"title": "t"}),
        })
        .unwrap();
    }

    #[test]
    fn worker_milestone_is_log_only_and_validates_payload() {
        let (_dir, mut l) = test_ledger();
        seeded_bead(&mut l, "gc-1");
        let seq = l
            .append(EventInput {
                kind: EventType::WorkerMilestone,
                rig: Some("gc".into()),
                actor: "t/dev/1".into(),
                bead: Some("gc-1".into()),
                data: serde_json::json!({"text": "tests passing"}),
            })
            .unwrap();
        assert!(seq > 0);
        // no bead: still fine (a general breadcrumb)
        l.append(EventInput {
            kind: EventType::WorkerMilestone,
            rig: None,
            actor: "cli".into(),
            bead: None,
            data: serde_json::json!({"text": "note"}),
        })
        .unwrap();
        // empty text rejected, nothing appended
        let before = l.events_range(1, None).unwrap().len();
        let err = l.append(EventInput {
            kind: EventType::WorkerMilestone,
            rig: None,
            actor: "cli".into(),
            bead: None,
            data: serde_json::json!({"text": ""}),
        });
        assert!(err.is_err());
        // unknown bead rejected
        let err = l.append(EventInput {
            kind: EventType::WorkerMilestone,
            rig: None,
            actor: "cli".into(),
            bead: Some("gc-999".into()),
            data: serde_json::json!({"text": "x"}),
        });
        assert!(err.is_err());
        assert_eq!(l.events_range(1, None).unwrap().len(), before);
    }

    #[test]
    fn worktree_events_are_log_only_and_validate_payloads() {
        let (_dir, mut l) = test_ledger();
        seeded_bead(&mut l, "gc-1");
        l.append(EventInput {
            kind: EventType::WorktreeKept,
            rig: Some("gc".into()),
            actor: "campd".into(),
            bead: Some("gc-1".into()),
            data: serde_json::json!({"path": "/camp/worktrees/gc-1", "reason": "outcome fail"}),
        })
        .unwrap();
        l.append(EventInput {
            kind: EventType::BeadWorktreeReaped,
            rig: Some("gc".into()),
            actor: "campd".into(),
            bead: Some("gc-1".into()),
            data: serde_json::json!({"path": "/camp/worktrees/gc-1"}),
        })
        .unwrap();
        // missing bead is an error for both
        for (kind, data) in [
            (EventType::WorktreeKept, serde_json::json!({"path": "/p", "reason": "r"})),
            (EventType::BeadWorktreeReaped, serde_json::json!({"path": "/p"})),
        ] {
            let err = l.append(EventInput {
                kind,
                rig: None,
                actor: "campd".into(),
                bead: None,
                data,
            });
            assert!(err.is_err(), "{kind:?} without a bead must fail");
        }
    }

    #[test]
    fn dispatch_failed_requires_bead_and_reason() {
        let (_dir, mut l) = test_ledger();
        seeded_bead(&mut l, "gc-1");
        l.append(EventInput {
            kind: EventType::DispatchFailed,
            rig: Some("gc".into()),
            actor: "campd".into(),
            bead: Some("gc-1".into()),
            data: serde_json::json!({"reason": "no agent named \"dev\""}),
        })
        .unwrap();
        let err = l.append(EventInput {
            kind: EventType::DispatchFailed,
            rig: Some("gc".into()),
            actor: "campd".into(),
            bead: Some("gc-1".into()),
            data: serde_json::json!({"reason": ""}),
        });
        assert!(err.is_err(), "empty reason must fail");
    }

    #[test]
    fn session_woke_accepts_worktree_and_session_end_accepts_exit_details() {
        let (_dir, mut l) = test_ledger();
        seeded_bead(&mut l, "gc-1");
        l.append(EventInput {
            kind: EventType::SessionWoke,
            rig: Some("gc".into()),
            actor: "campd".into(),
            bead: Some("gc-1".into()),
            data: serde_json::json!({
                "name": "t/dev/1", "agent": "dev", "rig": "gc",
                "claude_session_id": "7bd2befc-b018-4080-8738-429d541b3646",
                "transcript_path": "/home/u/.claude/projects/-x/7bd2befc.jsonl",
                "bead": "gc-1",
                "worktree": "/camp/worktrees/gc-1"
            }),
        })
        .unwrap();
        l.append(EventInput {
            kind: EventType::SessionCrashed,
            rig: Some("gc".into()),
            actor: "campd".into(),
            bead: None,
            data: serde_json::json!({"name": "t/dev/1", "exit_code": 7}),
        })
        .unwrap();
        // signal + reason variants also parse (fresh session to end)
        l.append(EventInput {
            kind: EventType::SessionWoke,
            rig: None,
            actor: "campd".into(),
            bead: None,
            data: serde_json::json!({"name": "t/dev/2", "agent": "dev"}),
        })
        .unwrap();
        l.append(EventInput {
            kind: EventType::SessionCrashed,
            rig: None,
            actor: "campd".into(),
            bead: None,
            data: serde_json::json!({"name": "t/dev/2", "signal": 9, "reason": "spawn failed: ..."}),
        })
        .unwrap();
    }
```

If `ledger/mod.rs`'s test module lacks a `test_ledger()` helper, reuse whatever open-a-tempdir-ledger helper it has (Phase 1 wrote one; match its name — the executor should adapt these tests to the existing helper rather than duplicating it).

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --package camp-core ledger`
Expected: FAIL — `WorkerMilestone` etc. do not exist (compile error).

- [ ] **Step 3: Implement.**

`crates/camp-core/src/event.rs` — add four variants (enum, `ALL`, `as_str`):

```rust
    WorkerMilestone,
    WorktreeKept,
    BeadWorktreeReaped,
    DispatchFailed,
```
```rust
            EventType::WorkerMilestone => "worker.milestone",
            EventType::WorktreeKept => "worktree.kept",
            EventType::BeadWorktreeReaped => "bead.worktree.reaped",
            EventType::DispatchFailed => "dispatch.failed",
```

`crates/camp-core/src/vocab.rs`:

```rust
// GC_MIRRORED_EVENTS gains:
    "bead.worktree.reaped",
// CAMP_SPECIFIC_EVENTS gains:
    "worker.milestone",
    "worktree.kept",
    "dispatch.failed",
```

`crates/camp-core/src/ledger/fold.rs` — new arms in `apply`:

```rust
        EventType::WorkerMilestone => worker_milestone(conn, event),
        EventType::WorktreeKept => worktree_kept(conn, event),
        EventType::BeadWorktreeReaped => bead_worktree_reaped(conn, event),
        EventType::DispatchFailed => dispatch_failed(conn, event),
```

and implementations (log-only; validation only — the run's durable truth stays in existing tables):

```rust
fn known_bead(conn: &Connection, event: &Event, id: &str) -> Result<(), CoreError> {
    if bead_status(conn, id)?.is_none() {
        return Err(CoreError::UnknownBead(id.to_owned()));
    }
    let _ = event;
    Ok(())
}

fn non_empty(event: &Event, field: &str, value: &str) -> Result<(), CoreError> {
    if value.is_empty() {
        return Err(CoreError::InvalidEventData {
            event_type: event.kind.as_str().to_owned(),
            reason: format!("empty {field}"),
        });
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkerMilestone {
    text: String,
}

/// `worker.milestone` is log-only: worker breadcrumbs (spec §8.1). The bead
/// is optional; when named it must exist (fail fast on typos).
fn worker_milestone(conn: &Connection, event: &Event) -> Result<(), CoreError> {
    let p: WorkerMilestone = payload(event)?;
    non_empty(event, "text", &p.text)?;
    if let Some(bead) = event.bead.as_deref() {
        known_bead(conn, event, bead)?;
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorktreeKept {
    path: String,
    reason: String,
}

/// `worktree.kept` is log-only: a failed bead's worktree stays for
/// forensics (spec §12), and the ledger records where and why.
fn worktree_kept(conn: &Connection, event: &Event) -> Result<(), CoreError> {
    let bead = required_bead(event)?;
    known_bead(conn, event, bead)?;
    let p: WorktreeKept = payload(event)?;
    non_empty(event, "path", &p.path)?;
    non_empty(event, "reason", &p.reason)?;
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BeadWorktreeReaped {
    path: String,
}

/// `bead.worktree.reaped` (gc-mirrored name) is log-only: a clean close's
/// worktree was removed (spec §12).
fn bead_worktree_reaped(conn: &Connection, event: &Event) -> Result<(), CoreError> {
    let bead = required_bead(event)?;
    known_bead(conn, event, bead)?;
    let p: BeadWorktreeReaped = payload(event)?;
    non_empty(event, "path", &p.path)?;
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DispatchFailed {
    reason: String,
}

/// `dispatch.failed` is log-only: campd could not dispatch a ready bead
/// (unresolvable agent, missing rig, worktree failure). campd has no
/// caller, so the error lands here (invariant 5); plan decision F.
fn dispatch_failed(conn: &Connection, event: &Event) -> Result<(), CoreError> {
    let bead = required_bead(event)?;
    known_bead(conn, event, bead)?;
    let p: DispatchFailed = payload(event)?;
    non_empty(event, "reason", &p.reason)?;
    Ok(())
}
```

Extend the session payload structs (additive, still deny_unknown_fields):

```rust
struct SessionWoke {
    // ... existing fields unchanged ...
    #[serde(default)]
    worktree: Option<String>,
}
```
(`session_woke` ignores `p.worktree` beyond parsing — no sessions column exists and schema v1 is frozen; the value is ledger audit + campd memory.)

```rust
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionEnd {
    name: String,
    #[serde(default)]
    exit_code: Option<i64>,
    #[serde(default)]
    signal: Option<i64>,
    #[serde(default)]
    reason: Option<String>,
}
```
(`session_ended` keeps using only `p.name`; the extras are audit fields recording F4 evidence.)

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --package camp-core`
Expected: PASS — including `vocab_pin` (partition covers the four new names automatically via `EventType::ALL`) and `refold_prop` (untouched generator still green).

- [ ] **Step 5: Commit**

```bash
git add crates/camp-core/src/event.rs crates/camp-core/src/vocab.rs crates/camp-core/src/ledger
git commit -m "feat: worker.milestone, worktree.kept, bead.worktree.reaped, dispatch.failed events"
```

### Task 3: camp-core queries — `dispatchable_beads`, `next_session_name`

**Files:**
- Modify: `crates/camp-core/src/readiness.rs`, `crates/camp-core/src/ledger/mod.rs`

**Interfaces:**
- Consumes: `BeadRow`, `BEAD_COLS`, `UNMET_DEP`, `row_to_bead`, `collect` (readiness.rs internals), `Ledger` internals.
- Produces:

```rust
// readiness.rs
pub fn dispatchable_beads(conn: &Connection) -> Result<Vec<BeadRow>, CoreError>;
// ledger/mod.rs
impl Ledger {
    pub fn dispatchable_beads(&self) -> Result<Vec<BeadRow>, CoreError>;
    /// "<camp>/<agent>/<n>": n = 1 + max existing suffix for this exact prefix.
    pub fn next_session_name(&self, camp: &str, agent: &str) -> Result<String, CoreError>;
}
```

- [ ] **Step 1: Write the failing tests** — append to `crates/camp-core/src/readiness.rs`'s test module (it exists; reuse its helpers for creating beads — adapt names to what is there):

```rust
    #[test]
    fn dispatchable_excludes_blocked_closed_nontask_roots_and_sessioned() {
        let (_dir, mut l) = test_ledger();
        // plain ready task: IN
        create(&mut l, "gc-1", &[]);
        // blocked: OUT
        create(&mut l, "gc-2", &["gc-1"]);
        // memory bead: OUT
        l.append(EventInput {
            kind: EventType::BeadCreated,
            rig: Some("gc".into()),
            actor: "t".into(),
            bead: Some("gc-3".into()),
            data: serde_json::json!({"title": "fact", "type": "memory"}),
        })
        .unwrap();
        // run root (run_id, no step_id): OUT — Phase 9 finalizes roots
        l.append(EventInput {
            kind: EventType::BeadCreated,
            rig: Some("gc".into()),
            actor: "t".into(),
            bead: Some("gc-4".into()),
            data: serde_json::json!({"title": "root", "run_id": "r1"}),
        })
        .unwrap();
        // run STEP (run_id + step_id): IN — steps are worker work
        l.append(EventInput {
            kind: EventType::BeadCreated,
            rig: Some("gc".into()),
            actor: "t".into(),
            bead: Some("gc-5".into()),
            data: serde_json::json!({"title": "step", "run_id": "r1", "step_id": "s1"}),
        })
        .unwrap();
        // bead with a session bound (dispatched already): OUT
        create(&mut l, "gc-6", &[]);
        l.append(EventInput {
            kind: EventType::SessionWoke,
            rig: Some("gc".into()),
            actor: "campd".into(),
            bead: None,
            data: serde_json::json!({"name": "t/dev/1", "agent": "dev", "bead": "gc-6"}),
        })
        .unwrap();
        let ids: Vec<String> = l
            .dispatchable_beads()
            .unwrap()
            .into_iter()
            .map(|b| b.id)
            .collect();
        assert_eq!(ids, vec!["gc-1", "gc-5"]);
    }

    #[test]
    fn dispatchable_still_excludes_after_bound_session_ends() {
        // Phase 8 never respawns (plan decision C): a bead whose session
        // crashed goes back to open but is NOT re-dispatchable until the
        // Phase 9/11 retry machinery exists.
        let (_dir, mut l) = test_ledger();
        create(&mut l, "gc-1", &[]);
        l.append(EventInput {
            kind: EventType::SessionWoke,
            rig: Some("gc".into()),
            actor: "campd".into(),
            bead: None,
            data: serde_json::json!({"name": "t/dev/1", "agent": "dev", "bead": "gc-1"}),
        })
        .unwrap();
        l.append(EventInput {
            kind: EventType::SessionCrashed,
            rig: Some("gc".into()),
            actor: "campd".into(),
            bead: None,
            data: serde_json::json!({"name": "t/dev/1"}),
        })
        .unwrap();
        assert!(l.dispatchable_beads().unwrap().is_empty());
    }
```

And in `crates/camp-core/src/ledger/mod.rs` tests:

```rust
    #[test]
    fn next_session_name_allocates_per_camp_and_agent() {
        let (_dir, mut l) = test_ledger();
        assert_eq!(l.next_session_name("t", "dev").unwrap(), "t/dev/1");
        l.append(EventInput {
            kind: EventType::SessionWoke,
            rig: None,
            actor: "campd".into(),
            bead: None,
            data: serde_json::json!({"name": "t/dev/1", "agent": "dev"}),
        })
        .unwrap();
        l.append(EventInput {
            kind: EventType::SessionWoke,
            rig: None,
            actor: "campd".into(),
            bead: None,
            data: serde_json::json!({"name": "t/dev/7", "agent": "dev"}),
        })
        .unwrap();
        // other agents and other camps do not collide
        l.append(EventInput {
            kind: EventType::SessionWoke,
            rig: None,
            actor: "campd".into(),
            bead: None,
            data: serde_json::json!({"name": "other/dev/40", "agent": "dev"}),
        })
        .unwrap();
        assert_eq!(l.next_session_name("t", "dev").unwrap(), "t/dev/8");
        assert_eq!(l.next_session_name("t", "reviewer").unwrap(), "t/reviewer/1");
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --package camp-core readiness ledger`
Expected: FAIL — methods do not exist.

- [ ] **Step 3: Implement.**

`crates/camp-core/src/readiness.rs`:

```rust
/// Beads campd may dispatch a worker for (Phase 8, plan decision C): open,
/// ready (decision-6 rule), plain work (`type='task'`), not a run root
/// (roots are finalized by campd, Phase 9), and never dispatched before
/// (no sessions row bound — respawn-after-crash arrives with retry
/// budgets, Phase 9/11). Oldest first, like `ready_beads`.
pub fn dispatchable_beads(conn: &Connection) -> Result<Vec<BeadRow>, CoreError> {
    let sql = format!(
        "SELECT {BEAD_COLS} FROM beads b
         WHERE b.status = 'open' AND b.type = 'task'
           AND NOT (b.run_id IS NOT NULL AND b.step_id IS NULL)
           AND NOT EXISTS (SELECT 1 FROM sessions s WHERE s.bead = b.id)
           AND NOT EXISTS (
             SELECT 1 FROM deps d LEFT JOIN beads t ON t.id = d.needs_id
             WHERE d.bead_id = b.id AND {UNMET_DEP})
         ORDER BY b.created_ts, b.id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], row_to_bead)?;
    collect(rows)
}
```

`crates/camp-core/src/ledger/mod.rs` (next to the other readiness wrappers):

```rust
    pub fn dispatchable_beads(&self) -> Result<Vec<crate::readiness::BeadRow>, CoreError> {
        crate::readiness::dispatchable_beads(&self.conn)
    }

    /// Allocate the next session name `<camp>/<agent>/<n>` (spec §7.4,
    /// master plan Phase 8). n = 1 + the highest existing suffix among
    /// sessions with this exact prefix; suffix parsing happens in Rust so
    /// odd agent names cannot break a LIKE pattern. Only campd allocates
    /// in v1; the fold's duplicate-name rejection backstops any race.
    pub fn next_session_name(&self, camp: &str, agent: &str) -> Result<String, CoreError> {
        let prefix = format!("{camp}/{agent}/");
        let mut stmt = self
            .conn
            .prepare("SELECT name FROM sessions WHERE agent = ?1")?;
        let names = stmt.query_map([agent], |r| r.get::<_, String>(0))?;
        let mut max_n: i64 = 0;
        for name in names {
            let name = name?;
            if let Some(rest) = name.strip_prefix(&prefix)
                && let Ok(n) = rest.parse::<i64>()
            {
                max_n = max_n.max(n);
            }
        }
        Ok(format!("{prefix}{}", max_n + 1))
    }
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --package camp-core`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/camp-core/src/readiness.rs crates/camp-core/src/ledger/mod.rs
git commit -m "feat: dispatchable-beads query and session name allocation"
```


### Task 4: `camp-core/src/pack.rs` — agent definitions and last-wins layering

**Files:**
- Create: `crates/camp-core/src/pack.rs`
- Modify: `crates/camp-core/src/lib.rs` (`pub mod pack;`), `crates/camp-core/src/error.rs` (two variants), `crates/camp-core/Cargo.toml` (YAML dep)

**Interfaces:**
- Consumes: `CampConfig` (Task 1 — `packs`, `root`), `CoreError`.
- Produces (master-plan-pinned signatures):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Isolation { #[default] None, Worktree }

#[derive(Debug, Clone, PartialEq)]
pub struct AgentDef {
    pub name: String,
    pub model: Option<String>,
    pub tools: Option<Vec<String>>,
    pub permission_mode: Option<String>,
    pub isolation: Isolation,
    pub prompt: String,
}

pub fn parse_agent_file(path: &Path) -> Result<AgentDef, CoreError>;
pub fn resolve_agent(cfg: &CampConfig, name: &str) -> Result<AgentDef, CoreError>;
```

**Format facts (decision A), VERIFIED against the official sub-agents documentation (code.claude.com/docs/en/sub-agents, retrieved 2026-07-07) and 12 installed agent files on this machine:** agent files are Claude Code agent definitions — YAML frontmatter between `---` fences, then the prompt body ("the body becomes the system prompt"). The documented field list is `name` (required — "identity comes only from the `name` frontmatter field; the filename doesn't have to match"), `description` (required by Claude Code for delegation routing), `tools`, `disallowedTools`, `model` (`sonnet`/`opus`/`haiku`/`fable`, a full model id, or `inherit`), `permissionMode` (exact spelling; values `default`, `acceptEdits`, `auto`, `dontAsk`, `bypassPermissions`, `plan`), `maxTurns`, `skills`, `mcpServers`, `hooks`, `memory`, `background`, `effort`, `isolation` (`worktree`), `color`, `initialPrompt`.

Camp reads: `name` (string, **required** — a hard error naming the file when absent, matching Claude Code's identity rule), `tools` (comma-separated string — the form every installed file uses — or a YAML list), `model` (string, passed through unvalidated; `claude` itself rejects bad values visibly at spawn), `permissionMode` (string, passed through as `--permission-mode`), `isolation` (`"worktree"` → `Isolation::Worktree`; any other value is an error naming the accepted value). Everything else — including `description`, which camp does not consume — is tolerated and ignored: Claude Code owns this format and grows it (decision A); the keys camp reads are type-checked strictly. One documented nuance recorded for reviewers: Claude Code ignores `permissionMode` for *plugin*-scoped subagents for security; camp honors it from pack files because packs are user-declared configuration in camp.toml (spec §11) and F7 requires per-agent permission pinning — the trust decision is the operator's, made visibly in config.

- [ ] **Step 1: Add the YAML dependency and error variants**

```bash
cargo add --package camp-core yaml-rust2
```

In `crates/camp-core/src/error.rs`:

```rust
    #[error("pack: {0}")]
    Pack(String),
    #[error("unknown agent {name:?}; searched {searched:?} (packs in camp.toml order, then <camp>/agents/)")]
    UnknownAgent { name: String, searched: Vec<String> },
```

- [ ] **Step 2: Write the failing tests** — `crates/camp-core/src/pack.rs` with a `tests` module (file created in this step; `lib.rs` gains `pub mod pack;`):

```rust
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::config::CampConfig;
    use std::path::Path;

    fn write_agent(dir: &Path, file: &str, content: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(file), content).unwrap();
    }

    const DEV: &str = "---\nname: dev\ndescription: implements changes\ntools: Read, Edit, Bash\nmodel: sonnet\npermissionMode: acceptEdits\n---\nImplement the change with TDD.\n";

    #[test]
    fn parses_a_claude_code_agent_file() {
        let dir = tempfile::tempdir().unwrap();
        write_agent(dir.path(), "dev.md", DEV);
        let def = parse_agent_file(&dir.path().join("dev.md")).unwrap();
        assert_eq!(def.name, "dev");
        assert_eq!(def.model.as_deref(), Some("sonnet"));
        assert_eq!(
            def.tools,
            Some(vec!["Read".to_owned(), "Edit".to_owned(), "Bash".to_owned()])
        );
        assert_eq!(def.permission_mode.as_deref(), Some("acceptEdits"));
        assert_eq!(def.isolation, Isolation::None);
        assert_eq!(def.prompt, "Implement the change with TDD.");
    }

    #[test]
    fn tools_accepts_a_yaml_list_and_isolation_worktree_parses() {
        let dir = tempfile::tempdir().unwrap();
        write_agent(
            dir.path(),
            "iso.md",
            "---\nname: iso\ntools:\n  - Read\n  - Bash\nisolation: worktree\n---\nWork isolated.\n",
        );
        let def = parse_agent_file(&dir.path().join("iso.md")).unwrap();
        assert_eq!(def.tools, Some(vec!["Read".to_owned(), "Bash".to_owned()]));
        assert_eq!(def.isolation, Isolation::Worktree);
    }

    #[test]
    fn unknown_keys_are_tolerated_but_a_missing_name_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        // unknown/unread keys (description, color, maxTurns…) are Claude
        // Code's business — tolerated (decision A)
        write_agent(
            dir.path(),
            "quiet.md",
            "---\nname: quiet\ndescription: d\ncolor: cyan\nmaxTurns: 3\n---\nPrompt.\n",
        );
        let def = parse_agent_file(&dir.path().join("quiet.md")).unwrap();
        assert_eq!(def.name, "quiet");
        assert_eq!(def.prompt, "Prompt.");

        // name is required: identity comes only from the name field
        // (sub-agents docs), so a nameless file is a hard error
        write_agent(dir.path(), "anon.md", "---\ndescription: d\n---\nPrompt.\n");
        let err = parse_agent_file(&dir.path().join("anon.md")).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("name") && msg.contains("anon.md"),
            "error must name the missing key and the file: {msg}"
        );
    }

    #[test]
    fn malformed_files_fail_naming_the_file_and_problem() {
        let dir = tempfile::tempdir().unwrap();
        for (file, content, needle) in [
            ("nofm.md", "just a prompt\n", "frontmatter"),
            ("badiso.md", "---\nname: x\nisolation: bubble\n---\nP\n", "isolation"),
            ("badtools.md", "---\nname: x\ntools: 7\n---\nP\n", "tools"),
            ("empty.md", "---\nname: x\n---\n\n", "prompt"),
        ] {
            write_agent(dir.path(), file, content);
            let err = parse_agent_file(&dir.path().join(file)).unwrap_err();
            let msg = err.to_string();
            assert!(
                msg.contains(needle) && msg.contains(file),
                "{file}: error {msg:?} must name {needle:?} and the file"
            );
        }
    }

    #[test]
    fn resolve_agent_layers_packs_last_wins_with_local_agents_highest() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_agent(&root.join("pack-a/agents"), "dev.md", "---\nname: dev\n---\nFrom pack-a.\n");
        write_agent(&root.join("pack-a/agents"), "only-a.md", "---\nname: only-a\n---\nA only.\n");
        write_agent(&root.join("pack-b/agents"), "dev.md", "---\nname: dev\n---\nFrom pack-b.\n");
        std::fs::write(
            root.join("camp.toml"),
            "[camp]\nname = \"t\"\npacks = [\"pack-a\", \"pack-b\"]\n",
        )
        .unwrap();
        let cfg = CampConfig::load(&root.join("camp.toml")).unwrap();

        // later pack wins
        assert_eq!(resolve_agent(&cfg, "dev").unwrap().prompt, "From pack-b.");
        // earlier pack still contributes what later layers don't override
        assert_eq!(resolve_agent(&cfg, "only-a").unwrap().prompt, "A only.");

        // local <camp>/agents/ beats every pack
        write_agent(&root.join("agents"), "dev.md", "---\nname: dev\n---\nLocal.\n");
        assert_eq!(resolve_agent(&cfg, "dev").unwrap().prompt, "Local.");

        // unknown agent: error lists the searched layers
        let err = resolve_agent(&cfg, "ghost").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("ghost") && msg.contains("pack-a"), "msg: {msg}");
    }

    #[test]
    fn duplicate_agent_names_in_one_layer_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_agent(&root.join("agents"), "a.md", "---\nname: dev\n---\nOne.\n");
        write_agent(&root.join("agents"), "b.md", "---\nname: dev\n---\nTwo.\n");
        std::fs::write(root.join("camp.toml"), "[camp]\nname = \"t\"\n").unwrap();
        let cfg = CampConfig::load(&root.join("camp.toml")).unwrap();
        let err = resolve_agent(&cfg, "dev").unwrap_err();
        assert!(err.to_string().contains("dev"), "got {err}");
    }

    #[test]
    fn missing_pack_dir_is_a_hard_error_and_parse_only_config_has_no_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("camp.toml"),
            "[camp]\nname = \"t\"\npacks = [\"nope\"]\n",
        )
        .unwrap();
        let cfg = CampConfig::load(&root.join("camp.toml")).unwrap();
        assert!(resolve_agent(&cfg, "dev").is_err(), "missing pack dir must fail");

        let cfg2 = CampConfig::parse("[camp]\nname = \"t\"\npacks = [\"p\"]\n").unwrap();
        let err = resolve_agent(&cfg2, "dev").unwrap_err();
        assert!(err.to_string().contains("root"), "got {err}");
    }
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test --package camp-core pack`
Expected: FAIL — module functions absent (compile error).

- [ ] **Step 4: Implement** `crates/camp-core/src/pack.rs`:

```rust
//! Packs (spec §11): agent definitions are Claude Code agent files —
//! YAML frontmatter + prompt body — read verbatim (zero invented formats).
//! Resolution layers packs from camp.toml in order, later wins, with the
//! camp-local agents/ directory highest (plan decisions A and R). Unknown
//! frontmatter keys are tolerated (Claude Code owns that format and grows
//! it); the keys camp reads are type-checked strictly.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use yaml_rust2::{Yaml, YamlLoader};

use crate::config::CampConfig;
use crate::error::CoreError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Isolation {
    #[default]
    None,
    Worktree,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AgentDef {
    pub name: String,
    pub model: Option<String>,
    pub tools: Option<Vec<String>>,
    pub permission_mode: Option<String>,
    pub isolation: Isolation,
    pub prompt: String,
}

fn pack_err(path: &Path, reason: impl std::fmt::Display) -> CoreError {
    CoreError::Pack(format!("{}: {reason}", path.display()))
}

/// Parse one Claude Code agent definition file.
pub fn parse_agent_file(path: &Path) -> Result<AgentDef, CoreError> {
    let text = std::fs::read_to_string(path).map_err(|e| pack_err(path, format!("cannot read: {e}")))?;
    let rest = text
        .strip_prefix("---\n")
        .ok_or_else(|| pack_err(path, "missing frontmatter (expected a `---` fence on line 1)"))?;
    let (front, body) = rest
        .split_once("\n---\n")
        .or_else(|| rest.split_once("\n---\r\n"))
        .ok_or_else(|| pack_err(path, "unterminated frontmatter (no closing `---` fence)"))?;
    let docs = YamlLoader::load_from_str(front)
        .map_err(|e| pack_err(path, format!("frontmatter is not valid YAML: {e}")))?;
    let doc = docs.first().cloned().unwrap_or(Yaml::Null);

    let get_str = |key: &str| -> Result<Option<String>, CoreError> {
        match &doc[key] {
            Yaml::BadValue | Yaml::Null => Ok(None),
            Yaml::String(s) => Ok(Some(s.clone())),
            other => Err(pack_err(path, format!("frontmatter key {key:?} must be a string, got {other:?}"))),
        }
    };

    // Identity comes only from the name field (sub-agents docs) — required.
    let name = get_str("name")?
        .ok_or_else(|| pack_err(path, "missing required frontmatter key \"name\""))?;

    let tools = match &doc["tools"] {
        Yaml::BadValue | Yaml::Null => None,
        Yaml::String(s) => Some(
            s.split(',')
                .map(|t| t.trim().to_owned())
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>(),
        ),
        Yaml::Array(items) => {
            let mut tools = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    Yaml::String(s) => tools.push(s.trim().to_owned()),
                    other => {
                        return Err(pack_err(path, format!("frontmatter key \"tools\" list holds a non-string: {other:?}")));
                    }
                }
            }
            Some(tools)
        }
        other => {
            return Err(pack_err(path, format!("frontmatter key \"tools\" must be a string or list, got {other:?}")));
        }
    };

    let isolation = match get_str("isolation")?.as_deref() {
        None => Isolation::None,
        Some("worktree") => Isolation::Worktree,
        Some(other) => {
            return Err(pack_err(path, format!("frontmatter key \"isolation\" accepts only \"worktree\", got {other:?}")));
        }
    };

    let prompt = body.trim().to_owned();
    if prompt.is_empty() {
        return Err(pack_err(path, "empty prompt body — an agent definition must say what the agent does"));
    }

    Ok(AgentDef {
        name,
        model: get_str("model")?,
        tools,
        permission_mode: get_str("permissionMode")?,
        isolation,
        prompt,
    })
}

/// The agents/ layers to search, lowest to highest (plan decision R).
fn layers(cfg: &CampConfig) -> Result<Vec<PathBuf>, CoreError> {
    let mut layers = Vec::with_capacity(cfg.packs.len() + 1);
    let need_root = || {
        CoreError::Config(
            "config has no root directory (loaded via parse, not load) — cannot resolve pack paths".to_owned(),
        )
    };
    for pack in &cfg.packs {
        let dir = if pack.is_absolute() {
            pack.clone()
        } else {
            cfg.root.as_deref().ok_or_else(need_root)?.join(pack)
        };
        if !dir.is_dir() {
            return Err(CoreError::Config(format!(
                "pack directory {} (from camp.toml packs) does not exist",
                dir.display()
            )));
        }
        layers.push(dir.join("agents"));
    }
    if let Some(root) = cfg.root.as_deref() {
        layers.push(root.join("agents"));
    } else if cfg.packs.is_empty() {
        return Err(need_root());
    }
    Ok(layers)
}

/// One layer's agent definitions by name; duplicate names in a layer are a
/// hard error (fail fast — silent shadowing within one directory hides a
/// pack bug).
fn load_layer(dir: &Path) -> Result<BTreeMap<String, AgentDef>, CoreError> {
    let mut defs = BTreeMap::new();
    if !dir.is_dir() {
        return Ok(defs); // a pack without agents/ contributes nothing
    }
    let entries = std::fs::read_dir(dir).map_err(|e| pack_err(dir, format!("cannot read: {e}")))?;
    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| pack_err(dir, format!("cannot read entry: {e}")))?;
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "md") {
            paths.push(path);
        }
    }
    paths.sort();
    for path in paths {
        let def = parse_agent_file(&path)?;
        if let Some(previous) = defs.insert(def.name.clone(), def) {
            return Err(pack_err(
                dir,
                format!("two files define agent {:?} in one layer", previous.name),
            ));
        }
    }
    Ok(defs)
}

/// Resolve an agent by name across the configured layers, last wins
/// (spec §11; master plan Phase 8 pinned signature).
pub fn resolve_agent(cfg: &CampConfig, name: &str) -> Result<AgentDef, CoreError> {
    let layers = layers(cfg)?;
    let mut found: Option<AgentDef> = None;
    for dir in &layers {
        if let Some(def) = load_layer(dir)?.remove(name) {
            found = Some(def);
        }
    }
    found.ok_or_else(|| CoreError::UnknownAgent {
        name: name.to_owned(),
        searched: layers.iter().map(|p| p.display().to_string()).collect(),
    })
}
```

Note for the executor: `Yaml` doesn't implement `Display` for the error strings above — use `{other:?}` (Debug) exactly as written.

- [ ] **Step 5: Run to verify pass**

Run: `cargo test --package camp-core pack`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/camp-core/src/pack.rs crates/camp-core/src/lib.rs crates/camp-core/src/error.rs crates/camp-core/Cargo.toml Cargo.lock
git commit -m "feat: pack agent definitions with last-wins layering (spec §11)"
```


### Task 5: `camp event emit`

**Files:**
- Create: `crates/camp/src/cmd/event_emit.rs`, `crates/camp/tests/cli_event_emit.rs`
- Modify: `crates/camp/src/main.rs` (subcommand)

**Interfaces:**
- Consumes: `Ledger::{open, get_bead, append}`, `poke_best_effort`, `EventType::WorkerMilestone` (Task 2).
- Produces: `camp event emit <TEXT> [--bead ID] [--session NAME]` — appends `worker.milestone`; actor is the session name when given, else `"cli"` (patrol's Phase 11 activity-reset matches events by actor == session name); rig is the named bead's rig.

- [ ] **Step 1: Write the failing CLI test** — `crates/camp/tests/cli_event_emit.rs`:

```rust
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! camp event emit (master plan Phase 8): the worker contract's milestone verb.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_camp");

fn camp(root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .env_remove("CAMP_DIR")
        .arg("--camp")
        .arg(root)
        .args(args)
        .output()
        .unwrap()
}

fn init_camp_with_rig(dir: &Path) -> PathBuf {
    let status = Command::new(BIN)
        .env_remove("CAMP_DIR")
        .current_dir(dir)
        .arg("init")
        .status()
        .unwrap();
    assert!(status.success());
    let root = dir.join(".camp");
    let rig = dir.join("repo");
    std::fs::create_dir_all(&rig).unwrap();
    let out = camp(&root, &["rig", "add", rig.to_str().unwrap(), "--prefix", "gc", "--name", "gc"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    root
}

fn events_json(root: &Path) -> Vec<serde_json::Value> {
    let out = camp(root, &["events", "--json"]);
    assert!(out.status.success());
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn emit_appends_a_milestone_with_session_actor_and_bead_rig() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp_with_rig(dir.path());
    let out = camp(&root, &["create", "work it"]);
    assert!(out.status.success());
    let bead = String::from_utf8(out.stdout).unwrap().trim().to_owned();

    let out = camp(
        &root,
        &["event", "emit", "tests passing", "--bead", &bead, "--session", "t/dev/1"],
    );
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let events = events_json(&root);
    let milestone = events
        .iter()
        .find(|e| e["type"] == "worker.milestone")
        .expect("worker.milestone event");
    assert_eq!(milestone["actor"], "t/dev/1");
    assert_eq!(milestone["bead"], bead.as_str());
    assert_eq!(milestone["rig"], "gc");
    assert_eq!(milestone["data"]["text"], "tests passing");
}

#[test]
fn emit_without_bead_or_session_defaults_to_cli_actor() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp_with_rig(dir.path());
    let out = camp(&root, &["event", "emit", "general note"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let events = events_json(&root);
    let milestone = events.iter().find(|e| e["type"] == "worker.milestone").unwrap();
    assert_eq!(milestone["actor"], "cli");
    assert!(milestone.get("bead").is_none());
    assert!(milestone.get("rig").is_none());
}

#[test]
fn emit_for_an_unknown_bead_fails_and_appends_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp_with_rig(dir.path());
    let before = events_json(&root).len();
    let out = camp(&root, &["event", "emit", "x", "--bead", "gc-999"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("gc-999"));
    assert_eq!(events_json(&root).len(), before);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --package camp --test cli_event_emit`
Expected: FAIL — clap knows no `event` subcommand (exit 2 → status assertion fails).

- [ ] **Step 3: Implement.**

`crates/camp/src/cmd/event_emit.rs`:

```rust
use anyhow::{Result, bail};
use camp_core::event::{EventInput, EventType};
use camp_core::ledger::Ledger;

use crate::campdir::CampDir;

/// `camp event emit <text> [--bead b] [--session s]` (master plan Phase 8):
/// append a `worker.milestone` breadcrumb. The actor is the emitting
/// session's name when given — Phase 11's stall patrol resets a worker's
/// timer on any ledger event from that session, so attribution matters.
pub fn run(
    camp: &CampDir,
    text: String,
    bead: Option<String>,
    session: Option<String>,
) -> Result<()> {
    let mut ledger = Ledger::open(&camp.db_path())?;
    let rig = match bead.as_deref() {
        Some(id) => match ledger.get_bead(id)? {
            Some(row) => Some(row.rig),
            None => bail!("unknown bead {id}"),
        },
        None => None,
    };
    let seq = ledger.append(EventInput {
        kind: EventType::WorkerMilestone,
        rig,
        actor: session.unwrap_or_else(|| "cli".to_owned()),
        bead,
        data: serde_json::json!({ "text": text }),
    })?;
    crate::daemon::socket::poke_best_effort(&camp.socket_path(), seq);
    Ok(())
}
```

`crates/camp/src/main.rs` — add to the `cmd` module list `pub mod event_emit;`, to `Command`:

```rust
    /// Append events by hand (worker contract surface)
    Event {
        #[command(subcommand)]
        command: EventCommand,
    },
```

a new subcommand enum:

```rust
#[derive(Subcommand)]
enum EventCommand {
    /// Record a worker.milestone breadcrumb
    Emit {
        /// What just happened, one line
        text: String,
        /// The bead this milestone belongs to
        #[arg(long)]
        bead: Option<String>,
        /// Emitting session name (actor attribution)
        #[arg(long)]
        session: Option<String>,
    },
}
```

and the dispatch arm in `run`:

```rust
        Command::Event { command } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            match command {
                EventCommand::Emit { text, bead, session } => {
                    cmd::event_emit::run(&camp, text, bead, session)
                }
            }
        }
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --package camp --test cli_event_emit`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/camp/src/cmd/event_emit.rs crates/camp/src/main.rs crates/camp/tests/cli_event_emit.rs
git commit -m "feat: camp event emit — worker.milestone breadcrumbs"
```

### Task 6: `camp sling`

**Files:**
- Create: `crates/camp/src/cmd/sling.rs`, `crates/camp/tests/cli_sling.rs`
- Modify: `crates/camp/src/main.rs` (subcommand), `crates/camp/src/cmd/create.rs` (make `resolve_rig` `pub(crate)`)

**Interfaces:**
- Consumes: `resolve_rig` (create.rs), `pack::resolve_agent` (Task 4), `Ledger::{next_bead_id, append}`, `autostart::request_with_autostart`, `Request::Poke`.
- Produces: `camp sling "<title>" [--agent a] [--rig r]` — one `bead.created` with the routed agent stamped as `assignee`; ensures campd is up (auto-start) and poked. Prints the bead id.

**Routing (decision D), resolved at sling time and stamped:** `--agent` → rig's `default_agent` → `[dispatch].default_agent`; no winner ⇒ exit 1 with a message naming all three fixes; a winner that no pack layer defines ⇒ exit 1 from `resolve_agent` (fail at the prompt, not in the daemon).

- [ ] **Step 1: Write the failing CLI test** — `crates/camp/tests/cli_sling.rs`:

```rust
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! camp sling (spec §8.1 Tier 0; master plan Phase 8). The daemon-side
//! dispatch behavior lives in daemon_dispatch.rs; this file covers the
//! CLI surface: routing resolution, fail-fast messages, assignee stamping,
//! and the auto-start poke.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_camp");

fn camp(root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .env_remove("CAMP_DIR")
        .arg("--camp")
        .arg(root)
        .args(args)
        .output()
        .unwrap()
}

/// A camp with one rig and a config we control completely. `command` is
/// `true` so an auto-started daemon's dispatch spawn is harmless.
fn scaffold(dir: &Path, dispatch_default: Option<&str>, rig_default: Option<&str>) -> PathBuf {
    let root = dir.join(".camp");
    std::fs::create_dir_all(&root).unwrap();
    let rig = dir.join("repo");
    std::fs::create_dir_all(&rig).unwrap();
    let rig_line = rig_default
        .map(|a| format!("default_agent = \"{a}\"\n"))
        .unwrap_or_default();
    let dispatch_line = dispatch_default
        .map(|a| format!("default_agent = \"{a}\"\n"))
        .unwrap_or_default();
    std::fs::write(
        root.join("camp.toml"),
        format!(
            "[camp]\nname = \"t\"\n\n[[rigs]]\nname = \"gc\"\npath = \"{}\"\nprefix = \"gc\"\n{rig_line}\n[dispatch]\ncommand = \"true\"\n{dispatch_line}",
            rig.display()
        ),
    )
    .unwrap();
    // ledger must exist for any verb; init won't run over an existing dir,
    // so open it via a cheap verb-adjacent path: camp events --json creates
    // nothing — use camp-core directly instead.
    camp_core::ledger::Ledger::open(&root.join("camp.db")).unwrap();
    root
}

fn write_agent(root: &Path, name: &str) {
    let agents = root.join("agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(
        agents.join(format!("{name}.md")),
        format!("---\nname: {name}\n---\nDo the work.\n"),
    )
    .unwrap();
}

fn events_json(root: &Path) -> Vec<serde_json::Value> {
    let out = camp(root, &["events", "--json"]);
    assert!(out.status.success());
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn stop_campd(root: &Path) {
    // sling auto-starts campd; leave nothing running behind the test
    let _ = camp(root, &["stop"]);
}

#[test]
fn sling_with_no_route_fails_naming_all_three_fixes_and_creates_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let root = scaffold(dir.path(), None, None);
    let out = camp(&root, &["sling", "add a flag"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    for needle in ["--agent", "default_agent", "[dispatch]", "[[rigs]]"] {
        assert!(stderr.contains(needle), "stderr must name {needle}: {stderr}");
    }
    assert!(events_json(&root).is_empty(), "no bead may be created");
    assert!(!root.join("campd.sock").exists(), "no daemon may be started");
}

#[test]
fn sling_with_an_unresolvable_agent_fails_before_creating_anything() {
    let dir = tempfile::tempdir().unwrap();
    let root = scaffold(dir.path(), Some("dev"), None);
    // no agents/ dir at all: routing picks "dev" but no layer defines it
    let out = camp(&root, &["sling", "add a flag"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("dev"));
    assert!(events_json(&root).is_empty());
}

#[test]
fn sling_stamps_the_dispatch_default_agent_and_autostarts_campd() {
    let dir = tempfile::tempdir().unwrap();
    let root = scaffold(dir.path(), Some("dev"), None);
    write_agent(&root, "dev");
    let out = camp(&root, &["sling", "add a flag"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let bead = String::from_utf8(out.stdout).unwrap().trim().to_owned();
    assert_eq!(bead, "gc-1");
    let events = events_json(&root);
    let created = events.iter().find(|e| e["type"] == "bead.created").unwrap();
    assert_eq!(created["data"]["assignee"], "dev");
    assert_eq!(created["data"]["title"], "add a flag");
    assert!(
        events.iter().any(|e| e["type"] == "campd.autostarted"),
        "sling must bring the daemon up (spec §5): {events:?}"
    );
    stop_campd(&root);
}

#[test]
fn rig_default_agent_outranks_the_camp_wide_default() {
    let dir = tempfile::tempdir().unwrap();
    let root = scaffold(dir.path(), Some("dev"), Some("rigger"));
    write_agent(&root, "dev");
    write_agent(&root, "rigger");
    let out = camp(&root, &["sling", "review it", "--rig", "gc"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let events = events_json(&root);
    let created = events.iter().find(|e| e["type"] == "bead.created").unwrap();
    assert_eq!(created["data"]["assignee"], "rigger");
    stop_campd(&root);
}

#[test]
fn explicit_agent_flag_outranks_everything() {
    let dir = tempfile::tempdir().unwrap();
    let root = scaffold(dir.path(), Some("dev"), Some("rigger"));
    write_agent(&root, "dev");
    write_agent(&root, "rigger");
    write_agent(&root, "special");
    let out = camp(&root, &["sling", "x", "--agent", "special"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let events = events_json(&root);
    let created = events.iter().find(|e| e["type"] == "bead.created").unwrap();
    assert_eq!(created["data"]["assignee"], "special");
    stop_campd(&root);
}
```

Add `camp-core` as a dev-dependency of `camp` if it is not already (`cargo add --package camp --dev camp-core` — check `crates/camp/Cargo.toml`; Phase 1 may already have it for test seeding).

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --package camp --test cli_sling`
Expected: FAIL — no `sling` subcommand.

- [ ] **Step 3: Implement.**

In `crates/camp/src/cmd/create.rs`, change `fn resolve_rig` to `pub(crate) fn resolve_rig` (no body change).

`crates/camp/src/cmd/sling.rs`:

```rust
use anyhow::{Result, bail};
use camp_core::config::CampConfig;
use camp_core::event::{EventInput, EventType};
use camp_core::ledger::Ledger;
use camp_core::pack;

use crate::campdir::CampDir;
use crate::cmd::create::resolve_rig;
use crate::daemon::autostart;
use crate::daemon::socket::Request;

/// `camp sling "<title>" [--agent a] [--rig r]` (spec §8.1, Tier 0): one
/// `bead.created` with the routed agent stamped as assignee, then a poke
/// that auto-starts campd if needed — sling promises dispatch, so a
/// fire-and-forget poke is not enough (plan decision P). campd does the
/// spawning; the attended-teammate surface is Phase 12.
pub fn run(camp: &CampDir, title: String, agent: Option<String>, rig: Option<String>) -> Result<()> {
    let config = CampConfig::load(&camp.config_path())?;
    let rig_cfg = resolve_rig(&config, rig.as_deref())?;

    // Routing (plan decision D), resolved and validated NOW — a routing
    // hole should fail at the user's prompt, not inside the daemon.
    let agent_name = match agent
        .or_else(|| rig_cfg.default_agent.clone())
        .or_else(|| config.dispatch.default_agent.clone())
    {
        Some(name) => name,
        None => bail!(
            "no agent to route to: pass --agent <name>, set default_agent on [[rigs]] {:?}, \
             or set default_agent under [dispatch] in {}",
            rig_cfg.name,
            camp.config_path().display()
        ),
    };
    // The routed agent must actually resolve in the pack layers.
    pack::resolve_agent(&config, &agent_name)?;

    let rig_name = rig_cfg.name.clone();
    let prefix = rig_cfg.prefix.clone();
    let mut ledger = Ledger::open(&camp.db_path())?;
    let id = ledger.next_bead_id(&prefix)?;
    let seq = ledger.append(EventInput {
        kind: EventType::BeadCreated,
        rig: Some(rig_name),
        actor: "cli".into(),
        bead: Some(id.clone()),
        data: serde_json::json!({ "title": title, "assignee": agent_name }),
    })?;
    drop(ledger); // campd may need the write lock immediately

    autostart::request_with_autostart(camp, &Request::Poke { seq }, "sling")?;
    println!("{id}");
    Ok(())
}
```

`crates/camp/src/main.rs` — `pub mod sling;` in the cmd module, new variant:

```rust
    /// Sling a bead: create it and have campd dispatch a worker (Tier 0)
    Sling {
        /// Bead title — what needs doing
        title: String,
        /// Route to a specific pack agent (default: the rig's or camp's default_agent)
        #[arg(long)]
        agent: Option<String>,
        /// Rig (default: the only configured rig)
        #[arg(long)]
        rig: Option<String>,
    },
```

dispatch arm:

```rust
        Command::Sling { title, agent, rig } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            cmd::sling::run(&camp, title, agent, rig)
        }
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --package camp --test cli_sling`
Expected: PASS. (If the auto-start test flakes on a slow machine, the failure mode to investigate is the readiness-line handshake, not a sleep — there are no sleeps in this path.)

- [ ] **Step 5: Commit**

```bash
git add crates/camp/src/cmd/sling.rs crates/camp/src/cmd/create.rs crates/camp/src/main.rs crates/camp/tests/cli_sling.rs crates/camp/Cargo.toml Cargo.lock
git commit -m "feat: camp sling — Tier-0 create, route, and dispatch via campd"
```


### Task 7: `daemon/spawn.rs` — spawn mechanics per F1–F7

**Files:**
- Create: `crates/camp/src/daemon/spawn.rs`
- Modify: `crates/camp/src/daemon/mod.rs` (`pub mod spawn;`), `crates/camp/Cargo.toml` (uuid)

**Interfaces:**
- Consumes: `AgentDef`/`Isolation` (Task 4).
- Produces (Task 8 relies on these exact signatures):

```rust
pub struct SpawnSpec {
    pub session_name: String,
    pub claude_session_id: String,
    pub transcript_path: PathBuf,
    pub cwd: PathBuf,
    pub argv: Vec<OsString>,              // argv[0] = the [dispatch].command
    pub env: Vec<(String, String)>,       // CAMP_DIR, CAMP_BEAD, CAMP_SESSION
    pub stdout_path: PathBuf,             // <camp>/sessions/<munged>.json
    pub stderr_path: PathBuf,             // <camp>/sessions/<munged>.log
}
pub fn new_session_id() -> String;                                    // uuid v4 (F1)
pub fn claude_config_root() -> Result<PathBuf>;                       // $CLAUDE_CONFIG_DIR | $HOME/.claude
pub fn transcript_path_under(root: &Path, cwd: &Path, sid: &str) -> PathBuf;  // pure (F3)
pub fn munge(text: &str) -> String;                                   // non-alphanumeric → '-'
pub fn build_spec(command: &Path, agent: &AgentDef, camp_root: &Path, bead_id: &str,
                  session_name: &str, session_id: &str, transcript: &Path, cwd: &Path) -> SpawnSpec;
pub fn spawn(spec: &SpawnSpec) -> Result<std::process::Child>;        // stdin /dev/null (F5)
pub fn create_worktree(rig_path: &Path, worktrees_dir: &Path, bead_id: &str) -> Result<PathBuf>;
pub fn remove_worktree(rig_path: &Path, worktree: &Path) -> Result<()>;
```

- [ ] **Step 1: Add the uuid dependency**

```bash
cargo add --package camp uuid --features v4
```

- [ ] **Step 2: Write the failing tests** — bottom of the new `crates/camp/src/daemon/spawn.rs` (start the file with just the test module and stubs absent so the failure is a compile error, or write tests first in your editor and build):

```rust
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use camp_core::pack::{AgentDef, Isolation};
    use std::process::Command;

    fn full_agent() -> AgentDef {
        AgentDef {
            name: "dev".into(),
            model: Some("sonnet".into()),
            tools: Some(vec!["Read".into(), "Edit".into(), "Bash".into()]),
            permission_mode: Some("acceptEdits".into()),
            isolation: Isolation::None,
            prompt: "Implement with TDD.".into(),
        }
    }

    /// Exit criterion pinned as a test: real-claude spawn arguments match
    /// the F1–F7 fixture facts exactly.
    #[test]
    fn argv_matches_the_fixture_facts_for_a_fully_pinned_agent() {
        let sid = "7bd2befc-b018-4080-8738-429d541b3646";
        let spec = build_spec(
            Path::new("claude"),
            &full_agent(),
            Path::new("/camps/dev"),
            "gc-142",
            "dev/dev/1",
            sid,
            Path::new("/home/u/.claude/projects/-code-gc/x.jsonl"),
            Path::new("/code/gc"),
        );
        let argv: Vec<&str> = spec.argv.iter().map(|s| s.to_str().unwrap()).collect();
        // F2: json envelope; F1: pre-assigned session id; F7: per-agent pins;
        // decision L: agent prompt via --append-system-prompt, task via -p.
        assert_eq!(
            argv[..12].to_vec(),
            vec![
                "claude",
                "--output-format", "json",
                "--session-id", sid,
                "--model", "sonnet",
                "--permission-mode", "acceptEdits",
                "--allowedTools", "Read,Edit,Bash",
                "--append-system-prompt",
            ]
        );
        assert_eq!(argv[12], "Implement with TDD.");
        assert_eq!(argv[13], "-p");
        let task = argv[14];
        assert!(task.contains("camp claim gc-142 --session dev/dev/1"), "task: {task}");
        assert!(task.contains("camp close gc-142 --outcome"), "task: {task}");
        assert!(task.contains("camp event emit"), "task: {task}");
        assert_eq!(argv.len(), 15);

        assert_eq!(
            spec.env,
            vec![
                ("CAMP_DIR".to_owned(), "/camps/dev".to_owned()),
                ("CAMP_BEAD".to_owned(), "gc-142".to_owned()),
                ("CAMP_SESSION".to_owned(), "dev/dev/1".to_owned()),
            ]
        );
        // decision G: capture paths under <camp>/sessions/
        assert_eq!(spec.stdout_path, Path::new("/camps/dev/sessions/dev-dev-1.json"));
        assert_eq!(spec.stderr_path, Path::new("/camps/dev/sessions/dev-dev-1.log"));
    }

    #[test]
    fn undeclared_agent_fields_emit_no_flags() {
        let agent = AgentDef {
            name: "bare".into(),
            model: None,
            tools: None,
            permission_mode: None,
            isolation: Isolation::None,
            prompt: "P".into(),
        };
        let spec = build_spec(
            Path::new("claude"), &agent, Path::new("/c"), "gc-1", "t/bare/1", "sid",
            Path::new("/t.jsonl"), Path::new("/code"),
        );
        let argv: Vec<&str> = spec.argv.iter().map(|s| s.to_str().unwrap()).collect();
        for flag in ["--model", "--permission-mode", "--allowedTools"] {
            assert!(!argv.contains(&flag), "{flag} must be absent: {argv:?}");
        }
        assert!(argv.contains(&"--append-system-prompt"));
    }

    /// F3, pinned against the Phase 2 D3 probe evidence shape.
    #[test]
    fn transcript_path_munges_every_non_alphanumeric_to_dash() {
        assert_eq!(munge("/tmp/rig-a"), "-tmp-rig-a");
        assert_eq!(munge("/code/gas_camp.rs"), "-code-gas-camp-rs");
        let p = transcript_path_under(
            Path::new("/home/u/.claude"),
            Path::new("/private/tmp/rig-a"),
            "7bd2befc-b018-4080-8738-429d541b3646",
        );
        assert_eq!(
            p,
            Path::new("/home/u/.claude/projects/-private-tmp-rig-a/7bd2befc-b018-4080-8738-429d541b3646.jsonl")
        );
    }

    #[test]
    fn session_ids_are_v4_uuids_and_unique() {
        let a = new_session_id();
        let b = new_session_id();
        assert_ne!(a, b);
        assert_eq!(a.len(), 36);
        assert_eq!(a.as_bytes()[14], b'4', "uuid version nibble must be 4");
    }

    /// Worktree lifecycle against a real git repo (decision H).
    #[test]
    fn worktree_create_and_remove_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let rig = dir.path().join("rig");
        std::fs::create_dir_all(&rig).unwrap();
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.email", "t@t"],
            vec!["config", "user.name", "t"],
            vec!["commit", "--allow-empty", "-m", "init"],
        ] {
            let out = Command::new("git").arg("-C").arg(&rig).args(&args).output().unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        }
        let worktrees = dir.path().join("worktrees");
        let wt = create_worktree(&rig, &worktrees, "gc-7").unwrap();
        assert_eq!(wt, worktrees.join("gc-7"));
        assert!(wt.join(".git").exists(), "a worktree has a .git link file");
        // fresh branch named for the bead
        let out = Command::new("git").arg("-C").arg(&wt).args(["branch", "--show-current"]).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "camp/gc-7");
        // a second create for the same bead fails fast
        assert!(create_worktree(&rig, &worktrees, "gc-7").is_err());
        remove_worktree(&rig, &wt).unwrap();
        assert!(!wt.exists());
    }
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test --package camp spawn`
Expected: FAIL — module absent (compile error; add `pub mod spawn;` to `daemon/mod.rs` as part of Step 4).

- [ ] **Step 4: Implement** `crates/camp/src/daemon/spawn.rs`:

```rust
//! Worker spawn mechanics (spec §8.4, §12). The Phase 2 fixture facts
//! F1–F7 (docs/design/2026-07-06-assumption-findings.md) are BINDING here:
//! F1 pre-assigned --session-id, F2 --output-format json, F3 transcript
//! path from the WORKER's cwd, F5 stdin at /dev/null, F7 per-agent pinning
//! flags. Everything in this module is mechanical; roles live in packs.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use anyhow::{Context, Result, bail};
use camp_core::pack::AgentDef;

/// The worker-contract instructions (spec §8.4: claim → milestones →
/// close → exit). `{bead}` and `{session}` are substituted per spawn; the
/// richer worker *skill* is Phase 12 pack content — this is the mechanical
/// floor every campd-spawned worker gets.
const WORKER_CONTRACT: &str = "You are a Gas Camp worker session working exactly one bead.\n\
Contract, in order:\n\
1. Claim it: run `camp claim {bead} --session {session}`\n\
2. Read it: `camp show {bead}`\n\
3. Do the work in the current directory.\n\
4. As you hit milestones, record them: `camp event emit \"<one line>\" --bead {bead} --session {session}`\n\
5. Close it: `camp close {bead} --outcome pass --reason \"<one line>\"` (or --outcome fail)\n\
6. Exit. Do not start unrelated work. CAMP_DIR is already set for the camp CLI.\n";

fn task_prompt(bead_id: &str, session_name: &str) -> String {
    WORKER_CONTRACT
        .replace("{bead}", bead_id)
        .replace("{session}", session_name)
}

/// Every non-ASCII-alphanumeric byte becomes '-' — Claude Code's project
/// dir scheme (F3), reused for the sessions/ capture file names.
pub fn munge(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// F1: campd pre-assigns the claude session id.
pub fn new_session_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Where Claude Code keeps its state: $CLAUDE_CONFIG_DIR override, else
/// $HOME/.claude (F3). No HOME is a per-dispatch error, not a campd crash.
pub fn claude_config_root() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let home = std::env::var_os("HOME")
        .context("HOME is not set; cannot compute the worker transcript path (F3)")?;
    Ok(PathBuf::from(home).join(".claude"))
}

/// F3: `<root>/projects/<munge(cwd)>/<sid>.jsonl`, computed from the
/// WORKER's cwd — the worktree path when isolation is on.
pub fn transcript_path_under(root: &Path, worker_cwd: &Path, session_id: &str) -> PathBuf {
    root.join("projects")
        .join(munge(&worker_cwd.to_string_lossy()))
        .join(format!("{session_id}.jsonl"))
}

pub struct SpawnSpec {
    pub session_name: String,
    pub claude_session_id: String,
    pub transcript_path: PathBuf,
    pub cwd: PathBuf,
    pub argv: Vec<OsString>,
    pub env: Vec<(String, String)>,
    pub stdout_path: PathBuf,
    pub stderr_path: PathBuf,
}

/// Assemble the exec plan. Pure — no filesystem, no process. The argv is
/// asserted verbatim by tests against F1/F2/F7 and decision L.
#[allow(clippy::too_many_arguments)]
pub fn build_spec(
    command: &Path,
    agent: &AgentDef,
    camp_root: &Path,
    bead_id: &str,
    session_name: &str,
    session_id: &str,
    transcript_path: &Path,
    cwd: &Path,
) -> SpawnSpec {
    let mut argv: Vec<OsString> = vec![command.as_os_str().to_owned()];
    let mut arg = |s: &str| argv.push(OsString::from(s));
    arg("--output-format");
    arg("json"); // F2
    arg("--session-id");
    arg(session_id); // F1
    if let Some(model) = &agent.model {
        arg("--model");
        arg(model); // F7
    }
    if let Some(mode) = &agent.permission_mode {
        arg("--permission-mode");
        arg(mode); // F7
    }
    if let Some(tools) = &agent.tools {
        arg("--allowedTools");
        arg(&tools.join(",")); // F7 (comma-joined list form)
    }
    if !agent.prompt.is_empty() {
        arg("--append-system-prompt");
        arg(&agent.prompt); // decision L: the role prompt
    }
    arg("-p");
    arg(&task_prompt(bead_id, session_name)); // the task

    let sessions_dir = camp_root.join("sessions");
    let file_stem = munge(session_name);
    SpawnSpec {
        session_name: session_name.to_owned(),
        claude_session_id: session_id.to_owned(),
        transcript_path: transcript_path.to_owned(),
        cwd: cwd.to_owned(),
        argv,
        env: vec![
            ("CAMP_DIR".to_owned(), camp_root.to_string_lossy().into_owned()),
            ("CAMP_BEAD".to_owned(), bead_id.to_owned()),
            ("CAMP_SESSION".to_owned(), session_name.to_owned()),
        ],
        stdout_path: sessions_dir.join(format!("{file_stem}.json")),
        stderr_path: sessions_dir.join(format!("{file_stem}.log")),
    }
}

/// Exec the worker. stdin is /dev/null (F5 — an open non-pipe stdin costs
/// a 3 s sniff; stream-json stdin-held workers are the Phase 11 nudge
/// path). stdout/stderr go to the sessions/ capture files (decision G).
/// The child is deliberately not waited here: SIGCHLD-driven try_wait in
/// the dispatcher reaps it, and workers intentionally outlive a killed
/// campd (adoption, spec §8.5).
#[allow(clippy::zombie_processes)]
pub fn spawn(spec: &SpawnSpec) -> Result<Child> {
    let sessions_dir = spec
        .stdout_path
        .parent()
        .context("capture path has no parent")?;
    std::fs::create_dir_all(sessions_dir)
        .with_context(|| format!("creating {}", sessions_dir.display()))?;
    let stdout = std::fs::File::create(&spec.stdout_path)
        .with_context(|| format!("creating {}", spec.stdout_path.display()))?;
    let stderr = std::fs::File::create(&spec.stderr_path)
        .with_context(|| format!("creating {}", spec.stderr_path.display()))?;
    let mut cmd = Command::new(&spec.argv[0]);
    cmd.args(&spec.argv[1..])
        .current_dir(&spec.cwd)
        .stdin(Stdio::null()) // F5
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    for (key, value) in &spec.env {
        cmd.env(key, value);
    }
    cmd.spawn()
        .with_context(|| format!("spawning {}", spec.argv[0].to_string_lossy()))
}

/// `git worktree add -b camp/<bead> <camp>/worktrees/<bead>` (decision H).
/// A pre-existing directory or branch fails fast — bead ids are unique and
/// Phase 8 never respawns a bead.
pub fn create_worktree(rig_path: &Path, worktrees_dir: &Path, bead_id: &str) -> Result<PathBuf> {
    std::fs::create_dir_all(worktrees_dir)
        .with_context(|| format!("creating {}", worktrees_dir.display()))?;
    let dir = worktrees_dir.join(bead_id);
    if dir.exists() {
        bail!("worktree {} already exists", dir.display());
    }
    let out = Command::new("git")
        .arg("-C")
        .arg(rig_path)
        .args(["worktree", "add", "-b"])
        .arg(format!("camp/{bead_id}"))
        .arg(&dir)
        .output()
        .context("running git worktree add")?;
    if !out.status.success() {
        bail!(
            "git worktree add failed for {}: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(dir)
}

/// Remove a clean worktree (decision H). The camp/<bead> branch is left
/// standing — it may hold unpushed work; sweeping is Phase 11 policy.
pub fn remove_worktree(rig_path: &Path, worktree: &Path) -> Result<()> {
    let out = Command::new("git")
        .arg("-C")
        .arg(rig_path)
        .args(["worktree", "remove", "--force"])
        .arg(worktree)
        .output()
        .context("running git worktree remove")?;
    if !out.status.success() {
        bail!(
            "git worktree remove failed for {}: {}",
            worktree.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}
```

Add `pub mod spawn;` to `crates/camp/src/daemon/mod.rs`.

- [ ] **Step 5: Run to verify pass**

Run: `cargo test --package camp spawn`
Expected: PASS (5 tests).

- [ ] **Step 6: Commit**

```bash
git add crates/camp/src/daemon/spawn.rs crates/camp/src/daemon/mod.rs crates/camp/Cargo.toml Cargo.lock
git commit -m "feat: worker spawn mechanics pinned to fixture facts F1-F7"
```

### Task 8: `daemon/dispatch.rs` — the Dispatcher

**Files:**
- Create: `crates/camp/src/daemon/dispatch.rs`
- Modify: `crates/camp/src/daemon/mod.rs` (`pub mod dispatch;`), `crates/camp/src/campdir.rs` (`#[derive(Clone)]` + `worktrees_path()`/`sessions_path()` helpers if absent)

**Interfaces:**
- Consumes: `Ledger::{dispatchable_beads, next_session_name, get_bead, append}`, `pack::resolve_agent`, spawn API (Task 7), `CampDir`, `CampConfig`.
- Produces (Task 9 relies on these):

```rust
pub struct Dispatcher { /* camp, config, children, failed */ }
impl Dispatcher {
    pub fn new(camp: CampDir, config: CampConfig) -> Dispatcher;
    /// Spawn workers for dispatchable beads until the cap (decision B).
    pub fn converge(&mut self, ledger: &mut Ledger) -> Result<()>;
    /// SIGCHLD service: reap exited children, record session ends (F4),
    /// dispose worktrees (decision H).
    pub fn reap(&mut self, ledger: &mut Ledger) -> Result<()>;
}
```

- [ ] **Step 1: Add `#[derive(Clone)]` to `CampDir`** in `crates/camp/src/campdir.rs` (the dispatcher owns a copy), and add path helpers if the struct lacks them:

```rust
    pub fn worktrees_path(&self) -> PathBuf {
        self.root.join("worktrees")
    }
```

- [ ] **Step 2: Write the failing unit tests** — test module of the new `crates/camp/src/daemon/dispatch.rs`:

```rust
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use camp_core::config::{CampConfig, RigConfig};
    use camp_core::readiness::BeadRow;
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;

    /// F4 exit mapping, pinned as a table.
    #[test]
    fn classify_maps_exits_per_f4() {
        let cases = [
            (ExitStatus::from_raw(0), EventType::SessionStopped, Some(0), None),
            (ExitStatus::from_raw(7 << 8), EventType::SessionCrashed, Some(7), None),
            // SIGKILL: shells report 137, the wait status is signal 9 (F4)
            (ExitStatus::from_raw(9), EventType::SessionCrashed, None, Some(9)),
            (ExitStatus::from_raw(15), EventType::SessionCrashed, None, Some(15)),
        ];
        for (status, kind, code, signal) in cases {
            assert_eq!(classify(status), (kind, code, signal), "status {status:?}");
        }
    }

    fn bead(assignee: Option<&str>) -> BeadRow {
        BeadRow {
            id: "gc-1".into(),
            rig: "gc".into(),
            kind: "task".into(),
            title: "t".into(),
            status: "open".into(),
            assignee: assignee.map(str::to_owned),
            claimed_by: None,
            outcome: None,
            labels: vec![],
            created_ts: "2026-07-07T00:00:00Z".into(),
            updated_ts: "2026-07-07T00:00:00Z".into(),
        }
    }

    fn config(rig_default: Option<&str>, camp_default: Option<&str>) -> CampConfig {
        let mut cfg = CampConfig::parse("[camp]\nname = \"t\"\n").unwrap();
        cfg.rigs.push(RigConfig {
            name: "gc".into(),
            path: "/tmp".into(),
            prefix: "gc".into(),
            default_agent: rig_default.map(str::to_owned),
        });
        cfg.dispatch.default_agent = camp_default.map(str::to_owned);
        cfg
    }

    /// Decision D routing order.
    #[test]
    fn route_prefers_assignee_then_rig_then_dispatch_default() {
        let cfg = config(Some("rigger"), Some("dev"));
        assert_eq!(route(&bead(Some("special")), &cfg).unwrap(), "special");
        assert_eq!(route(&bead(None), &cfg).unwrap(), "rigger");
        let cfg = config(None, Some("dev"));
        assert_eq!(route(&bead(None), &cfg).unwrap(), "dev");
        let cfg = config(None, None);
        let err = route(&bead(None), &cfg).unwrap_err();
        for needle in ["default_agent", "[dispatch]", "[[rigs]]"] {
            assert!(err.contains(needle), "route error must name {needle}: {err}");
        }
    }
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test --package camp dispatch`
Expected: FAIL — module absent.

- [ ] **Step 4: Implement** `crates/camp/src/daemon/dispatch.rs`:

```rust
//! The dispatcher (spec §7.3, §8.3, §8.4): on every wake, converge the
//! ledger's dispatchable set onto live worker children, up to
//! [dispatch].max_workers. Query-driven from ledger truth (plan decision
//! B) — crash-only, no in-memory queue to lose. Every failure lands in
//! the ledger (`dispatch.failed`, `session.crashed`), never in a void:
//! campd has no caller (invariant 5).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::ExitStatus;

use anyhow::{Context, Result};
use camp_core::config::CampConfig;
use camp_core::event::{EventInput, EventType};
use camp_core::ledger::Ledger;
use camp_core::pack::{self, Isolation};
use camp_core::readiness::BeadRow;

use super::spawn::{self, SpawnSpec};
use crate::campdir::CampDir;

pub struct Dispatcher {
    camp: CampDir,
    config: CampConfig,
    /// Live children by pid. campd is the parent (spec §10.1) — SIGCHLD
    /// lands here and try_wait reaps.
    children: HashMap<u32, Worker>,
    /// Beads that failed to dispatch this campd lifetime (decision F):
    /// one dispatch.failed each, retried once per restart (crash-only).
    failed: HashSet<String>,
}

struct Worker {
    child: std::process::Child,
    session: String,
    bead: String,
    rig: String,
    rig_path: PathBuf,
    worktree: Option<PathBuf>,
}

/// Everything prepare() resolves before any side effect.
struct Prep {
    spec: SpawnSpec,
    agent_name: String,
    rig_path: PathBuf,
    make_worktree: bool,
}

/// Decision D: assignee → rig default_agent → [dispatch].default_agent.
/// The Err is a human-actionable reason destined for dispatch.failed.
fn route(bead: &BeadRow, config: &CampConfig) -> Result<String, String> {
    if let Some(assignee) = &bead.assignee {
        return Ok(assignee.clone());
    }
    let rig_default = config
        .rigs
        .iter()
        .find(|r| r.name == bead.rig)
        .and_then(|r| r.default_agent.clone());
    rig_default
        .or_else(|| config.dispatch.default_agent.clone())
        .ok_or_else(|| {
            format!(
                "no agent to dispatch to: bead has no assignee, [[rigs]] {:?} has no \
                 default_agent, and [dispatch] has no default_agent",
                bead.rig
            )
        })
}

/// F4: exit 0 → stopped; nonzero exit or death-by-signal → crashed. Tool
/// denials exit 0 (F4) — failure routing is the worker contract's close
/// outcome, never the exit code.
fn classify(status: ExitStatus) -> (EventType, Option<i64>, Option<i64>) {
    use std::os::unix::process::ExitStatusExt;
    match status.code() {
        Some(0) => (EventType::SessionStopped, Some(0), None),
        Some(code) => (EventType::SessionCrashed, Some(i64::from(code)), None),
        None => (
            EventType::SessionCrashed,
            None,
            status.signal().map(i64::from),
        ),
    }
}

impl Dispatcher {
    pub fn new(camp: CampDir, config: CampConfig) -> Dispatcher {
        Dispatcher {
            camp,
            config,
            children: HashMap::new(),
            failed: HashSet::new(),
        }
    }

    /// Dispatch until the cap or the well runs dry. Re-queries after every
    /// spawn: the just-committed session.woke removes the bead from the
    /// dispatchable set, so the ledger is the only bookkeeping.
    pub fn converge(&mut self, ledger: &mut Ledger) -> Result<()> {
        loop {
            if self.children.len() >= self.config.dispatch.max_workers {
                return Ok(());
            }
            let next = ledger
                .dispatchable_beads()?
                .into_iter()
                .find(|b| !self.failed.contains(&b.id));
            let Some(bead) = next else { return Ok(()) };
            self.dispatch_one(ledger, &bead)?;
        }
    }

    /// One bead → one worker. Per-bead failures append dispatch.failed and
    /// return Ok — a broken bead must not stall its neighbors; a ledger
    /// failure is the only Err.
    fn dispatch_one(&mut self, ledger: &mut Ledger, bead: &BeadRow) -> Result<()> {
        let prep = match self.prepare(ledger, bead) {
            Ok(prep) => prep,
            Err(reason) => {
                self.failed.insert(bead.id.clone());
                ledger.append(EventInput {
                    kind: EventType::DispatchFailed,
                    rig: Some(bead.rig.clone()),
                    actor: "campd".into(),
                    bead: Some(bead.id.clone()),
                    data: serde_json::json!({ "reason": reason }),
                })?;
                return Ok(());
            }
        };
        self.launch(ledger, bead, prep)
    }

    /// Resolve everything fallible that has no side effects; the worktree
    /// (the one side-effectful step) comes last so nothing needs undoing
    /// on earlier failures. Err is a reason string for dispatch.failed.
    fn prepare(&self, ledger: &mut Ledger, bead: &BeadRow) -> Result<Prep, String> {
        let agent_name = route(bead, &self.config)?;
        let agent = pack::resolve_agent(&self.config, &agent_name).map_err(|e| e.to_string())?;
        let rig = self
            .config
            .rig(&bead.rig)
            .map_err(|e| format!("bead's rig is not configured: {e}"))?;
        if !rig.path.is_dir() {
            return Err(format!(
                "rig {:?} path {} is not a directory",
                rig.name,
                rig.path.display()
            ));
        }
        let session_name = ledger
            .next_session_name(&self.config.camp.name, &agent.name)
            .map_err(|e| format!("session name allocation failed: {e}"))?;
        let session_id = spawn::new_session_id();
        let make_worktree = agent.isolation == Isolation::Worktree;
        let cwd = if make_worktree {
            self.camp.worktrees_path().join(&bead.id)
        } else {
            rig.path.clone()
        };
        let claude_root = spawn::claude_config_root().map_err(|e| e.to_string())?;
        let transcript = spawn::transcript_path_under(&claude_root, &cwd, &session_id);
        let spec = spawn::build_spec(
            &self.config.dispatch.command,
            &agent,
            &self.camp.root,
            &bead.id,
            &session_name,
            &session_id,
            &transcript,
            &cwd,
        );
        Ok(Prep {
            spec,
            agent_name: agent.name,
            rig_path: rig.path.clone(),
            make_worktree,
        })
    }

    /// Registry at birth, then exec (F1). A spawn failure after the woke
    /// row committed appends session.crashed with the reason — the row
    /// must never dangle live (decision F).
    fn launch(&mut self, ledger: &mut Ledger, bead: &BeadRow, prep: Prep) -> Result<()> {
        let worktree = if prep.make_worktree {
            match spawn::create_worktree(&prep.rig_path, &self.camp.worktrees_path(), &bead.id) {
                Ok(dir) => Some(dir),
                Err(e) => {
                    self.failed.insert(bead.id.clone());
                    ledger.append(EventInput {
                        kind: EventType::DispatchFailed,
                        rig: Some(bead.rig.clone()),
                        actor: "campd".into(),
                        bead: Some(bead.id.clone()),
                        data: serde_json::json!({ "reason": format!("{e:#}") }),
                    })?;
                    return Ok(());
                }
            }
        } else {
            None
        };

        let mut woke = serde_json::json!({
            "name": prep.spec.session_name,
            "agent": prep.agent_name,
            "rig": bead.rig,
            "claude_session_id": prep.spec.claude_session_id,
            "transcript_path": prep.spec.transcript_path,
            "bead": bead.id,
        });
        if let Some(wt) = &worktree {
            woke["worktree"] = serde_json::json!(wt);
        }
        ledger.append(EventInput {
            kind: EventType::SessionWoke,
            rig: Some(bead.rig.clone()),
            actor: "campd".into(),
            bead: Some(bead.id.clone()),
            data: woke,
        })?;

        match spawn::spawn(&prep.spec) {
            Ok(child) => {
                self.children.insert(
                    child.id(),
                    Worker {
                        child,
                        session: prep.spec.session_name,
                        bead: bead.id.clone(),
                        rig: bead.rig.clone(),
                        rig_path: prep.rig_path,
                        worktree,
                    },
                );
                Ok(())
            }
            Err(e) => {
                ledger.append(EventInput {
                    kind: EventType::SessionCrashed,
                    rig: Some(bead.rig.clone()),
                    actor: "campd".into(),
                    bead: None,
                    data: serde_json::json!({
                        "name": prep.spec.session_name,
                        "reason": format!("spawn failed: {e:#}"),
                    }),
                })?;
                if let Some(wt) = worktree {
                    ledger.append(EventInput {
                        kind: EventType::WorktreeKept,
                        rig: Some(bead.rig.clone()),
                        actor: "campd".into(),
                        bead: Some(bead.id.clone()),
                        data: serde_json::json!({
                            "path": wt,
                            "reason": "spawn failed before the worker ran",
                        }),
                    })?;
                }
                Ok(())
            }
        }
    }

    /// SIGCHLD service (decision I). A child whose ledger writes fail
    /// stays tracked — try_wait re-returns the exit status, so the next
    /// wake retries the record instead of losing the session end.
    pub fn reap(&mut self, ledger: &mut Ledger) -> Result<()> {
        let mut exited: Vec<(u32, ExitStatus)> = Vec::new();
        for (pid, worker) in &mut self.children {
            match worker.child.try_wait() {
                Ok(Some(status)) => exited.push((*pid, status)),
                Ok(None) => {}
                Err(e) => return Err(e).context("try_wait on a worker"),
            }
        }
        for (pid, status) in exited {
            let Some(worker) = self.children.get(&pid) else {
                continue;
            };
            let (kind, exit_code, signal) = classify(status);
            let mut data = serde_json::json!({ "name": worker.session });
            if let Some(code) = exit_code {
                data["exit_code"] = serde_json::json!(code);
            }
            if let Some(sig) = signal {
                data["signal"] = serde_json::json!(sig);
            }
            ledger.append(EventInput {
                kind,
                rig: Some(worker.rig.clone()),
                actor: "campd".into(),
                bead: None,
                data,
            })?;
            // The record landed; now it is safe to forget the child.
            let Some(worker) = self.children.remove(&pid) else {
                continue;
            };
            self.dispose_worktree(ledger, &worker)?;
        }
        Ok(())
    }

    /// Decision H: closed-pass ⇒ remove + bead.worktree.reaped (gc name);
    /// anything else ⇒ keep + worktree.kept with the reason. A failed
    /// removal keeps, with the git error as the reason — never silent.
    fn dispose_worktree(&self, ledger: &mut Ledger, worker: &Worker) -> Result<()> {
        let Some(worktree) = &worker.worktree else {
            return Ok(());
        };
        let closed_pass = ledger
            .get_bead(&worker.bead)?
            .is_some_and(|b| b.status == "closed" && b.outcome.as_deref() == Some("pass"));
        let (kind, data) = if closed_pass {
            match spawn::remove_worktree(&worker.rig_path, worktree) {
                Ok(()) => (
                    EventType::BeadWorktreeReaped,
                    serde_json::json!({ "path": worktree }),
                ),
                Err(e) => (
                    EventType::WorktreeKept,
                    serde_json::json!({
                        "path": worktree,
                        "reason": format!("removal failed: {e:#}"),
                    }),
                ),
            }
        } else {
            (
                EventType::WorktreeKept,
                serde_json::json!({
                    "path": worktree,
                    "reason": format!("bead {} did not close pass; kept for forensics", worker.bead),
                }),
            )
        };
        ledger.append(EventInput {
            kind,
            rig: Some(worker.rig.clone()),
            actor: "campd".into(),
            bead: Some(worker.bead.clone()),
            data,
        })?;
        Ok(())
    }
}
```

Add `pub mod dispatch;` to `crates/camp/src/daemon/mod.rs`.

- [ ] **Step 5: Run to verify pass**

Run: `cargo test --package camp dispatch`
Expected: PASS (2 unit tests; behavior coverage is Task 11's integration suite).

- [ ] **Step 6: Commit**

```bash
git add crates/camp/src/daemon/dispatch.rs crates/camp/src/daemon/mod.rs crates/camp/src/campdir.rs
git commit -m "feat: dispatcher — converge dispatchable beads onto workers up to the cap"
```


### Task 9: Daemon wiring — SIGCHLD self-pipe and converge hooks

**Files:**
- Modify: `crates/camp/src/daemon/mod.rs`, `crates/camp/src/daemon/event_loop.rs` (minimal, additive — see the coordination note in decision I), `crates/camp/Cargo.toml` (signal-hook)

**Interfaces:**
- Consumes: `Dispatcher` (Task 8), `cursor::catch_up`, `ReadinessProcessor`.
- Produces: `event_loop::run(listener, sigchld, socket_path, ledger, processor, dispatcher)`; token layout `0 = LISTENER, 2 = SIGCHLD, 3+ = connections` (`Token(1)` reserved for Phase 10's CONFIG_WATCH).

**Wiring contract (all call sites):**
- Startup (`daemon::run`): load `CampConfig` (hard error if camp.toml is broken — a daemon that cannot read its config must not pretend to be up), register the SIGCHLD pipe BEFORE any spawn, `catch_up` → drain `take_pending()` → `dispatcher.converge()` (fatal on ledger error, same policy as startup catch-up), then the readiness line, then the loop.
- Poke: existing `catch_up` + drain, then `dispatcher.converge` — a converge failure answers the poker with the error (and stderr), like a catch-up failure.
- SIGCHLD event: drain the pipe → `dispatcher.reap` → `catch_up` + drain → `dispatcher.converge`; errors go to stderr and the loop continues (a broken child must not take campd down; unrecorded exits are retried on the next wake because `try_wait` re-returns the status).
- `poll_timeout()` is untouched (`None`), and no other event_loop behavior changes.

- [ ] **Step 1: Add the dependency**

```bash
cargo add --package camp signal-hook
```

- [ ] **Step 2: Write the failing test** — the existing in-process daemon test in `crates/camp/src/daemon/mod.rs` proves campd still serves; add a dispatch-through-the-daemon test to it (in-process: signal handling is per-process, so THIS test only asserts config loading + converge-on-poke with an empty dispatchable set and a missing-config error; full child reaping is Task 11's real-process suite):

```rust
    #[test]
    fn daemon_with_a_broken_config_refuses_to_start() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".camp");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("camp.toml"), "[camp]\nname = \"t\"\nbogus = 1\n").unwrap();
        let camp = CampDir { root };
        let err = run(&camp).unwrap_err();
        assert!(err.to_string().contains("bogus"), "got {err:#}");
    }
```

(The two existing `daemon_serves_*` tests keep passing — their camp.toml is valid and their dispatchable set is empty, so converge is a no-op.)

- [ ] **Step 3: Run to verify failure**

Run: `cargo test --package camp daemon`
Expected: the new test FAILS, and its pre-implementation failure mode is a **hang**, not an assertion — today's `run` ignores camp.toml, binds the socket, and blocks in the event loop forever. Watch it red like this: `cargo test --package camp daemon_with_a_broken_config -- --exact` in a shell, observe no completion within a few seconds (that IS the defect: a daemon that starts despite broken config), then kill it (Ctrl-C) and implement. Do not skip this observation.

- [ ] **Step 4: Implement.**

`crates/camp/src/daemon/mod.rs` — `run` becomes:

```rust
pub fn run(camp: &CampDir) -> Result<()> {
    // A daemon that cannot read its own config must not pretend to be up.
    let config = camp_core::config::CampConfig::load(&camp.config_path())
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut ledger = Ledger::open(&camp.db_path())?;
    let socket_path = camp.socket_path();
    let std_listener = socket::bind_or_replace(&socket_path)?;
    std_listener
        .set_nonblocking(true)
        .context("setting the listener non-blocking")?;
    let listener = mio::net::UnixListener::from_std(std_listener);

    // SIGCHLD self-pipe (decision I), registered before any child can
    // exist so no exit can be missed. signal-hook's handler writes a byte;
    // the poll loop drains it. No unsafe anywhere.
    let (sigchld_read, sigchld_write) =
        std::os::unix::net::UnixStream::pair().context("creating the SIGCHLD pipe")?;
    signal_hook::low_level::pipe::register(signal_hook::consts::SIGCHLD, sigchld_write)
        .context("registering the SIGCHLD handler")?;
    sigchld_read
        .set_nonblocking(true)
        .context("setting the SIGCHLD pipe non-blocking")?;

    ledger.append(EventInput {
        kind: EventType::CampdStarted,
        rig: None,
        actor: "campd".into(),
        bead: None,
        data: serde_json::json!({}),
    })?;

    // Startup catch-up and the backlog dispatch are fatal on error: a
    // daemon that cannot process its backlog must not pretend to be up.
    let mut processor = ReadinessProcessor::default();
    cursor::catch_up(&mut ledger, &mut processor)?;
    let _newly_ready = processor.take_pending();
    let mut dispatcher = dispatch::Dispatcher::new(camp.clone(), config);
    dispatcher.converge(&mut ledger)?;

    let mut stdout = std::io::stdout();
    writeln!(stdout, "{READY_PREFIX}{}", socket_path.display()).context("announcing readiness")?;
    stdout.flush().context("flushing the readiness line")?;

    event_loop::run(
        listener,
        sigchld_read,
        &socket_path,
        &mut ledger,
        &mut processor,
        &mut dispatcher,
    )
}
```

(also `pub mod dispatch;` / `pub mod spawn;` from earlier tasks, and the existing module docs gain nothing else.)

`crates/camp/src/daemon/event_loop.rs` — the complete set of edits (keep everything else byte-identical; this is the Phase 10 overlap file):

1. Token constants and start value:

```rust
const LISTENER: Token = Token(0);
// Token(1) is reserved for phase-10-orders' CONFIG_WATCH pipe (lead
// coordination, 2026-07-07). Shared layout: 0 listener, 1 config watch,
// 2 SIGCHLD, 3+ connections.
const SIGCHLD: Token = Token(2);
```
and `let mut next_token = 3usize;`

2. `run` signature and SIGCHLD registration:

```rust
pub fn run(
    mut listener: UnixListener,
    sigchld: std::os::unix::net::UnixStream,
    socket_path: &Path,
    ledger: &mut Ledger,
    processor: &mut ReadinessProcessor,
    dispatcher: &mut Dispatcher,
) -> Result<()> {
```
with, after the listener registration:

```rust
    let mut sigchld = mio::net::UnixStream::from_std(sigchld);
    poll.registry()
        .register(&mut sigchld, SIGCHLD, Interest::READABLE)
        .context("registering the SIGCHLD pipe")?;
```
and the import `use super::dispatch::Dispatcher;`.

3. A SIGCHLD arm in the token match, before the catch-all:

```rust
                SIGCHLD => {
                    drain_signal_pipe(&mut sigchld)?;
                    // Reap → record ends → catch up → refill capacity.
                    // Errors are reported, never fatal: a broken child must
                    // not take campd down, and unrecorded exits are retried
                    // next wake (try_wait re-returns the status).
                    if let Err(error) = reap_and_refill(ledger, processor, dispatcher) {
                        eprintln!("campd: reap failed: {error:#}");
                    }
                }
```

4. Two helpers at the bottom of the file:

```rust
/// Drain the self-pipe (signal deliveries coalesce; one byte or many, one
/// sweep of try_wait covers them all).
fn drain_signal_pipe(stream: &mut UnixStream) -> Result<()> {
    let mut buf = [0u8; 64];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => return Ok(()), // write end lives in the signal handler; 0 is unreachable-but-safe
            Ok(_) => {}
            Err(e) if e.kind() == ErrorKind::WouldBlock => return Ok(()),
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(e).context("draining the SIGCHLD pipe"),
        }
    }
}

fn reap_and_refill(
    ledger: &mut Ledger,
    processor: &mut ReadinessProcessor,
    dispatcher: &mut Dispatcher,
) -> Result<()> {
    dispatcher.reap(ledger)?;
    cursor::catch_up(ledger, processor)?;
    let _newly_ready = processor.take_pending();
    dispatcher.converge(ledger)?;
    Ok(())
}
```

5. The Poke arm's success path becomes catch-up **then converge** (both errors answer the poker):

```rust
            Ok(Request::Poke { seq: _ }) => {
                // The poked seq is advisory; catch-up reads past the cursor
                // regardless. Dispatch converges after catch-up (Phase 8).
                // A processing error answers the poker, lands on stderr, and
                // leaves the cursor before the failing event — surfaced,
                // never skipped.
                let response = match cursor::catch_up(ledger, processor)
                    .map_err(anyhow::Error::from)
                    .and_then(|_| {
                        let _newly_ready = processor.take_pending();
                        dispatcher.converge(ledger)
                    }) {
                    Ok(()) => Response::Ok { ok: true },
                    Err(e) => {
                        eprintln!("campd: poke processing failed: {e:#}");
                        Response::Error {
                            ok: false,
                            error: format!("poke processing failed: {e}"),
                        }
                    }
                };
                respond(&mut conn.stream, &response)?;
            }
```

6. `serve_connection` and `drain_lines` gain the `dispatcher: &mut Dispatcher` parameter threaded through (their existing unit test constructs one: `let mut dispatcher = Dispatcher::new(CampDir { root: dir.path().to_path_buf() }, CampConfig::parse("[camp]\nname = \"t\"\n").unwrap());`).

- [ ] **Step 5: Run to verify pass**

Run: `cargo test --package camp daemon`
Expected: PASS — including the Phase 7 lifecycle tests (their empty camps converge to nothing) and the pipelined-backlog unit test.

- [ ] **Step 6: Commit**

```bash
git add crates/camp/src/daemon crates/camp/Cargo.toml Cargo.lock
git commit -m "feat: SIGCHLD self-pipe and dispatch convergence in the campd event loop"
```

### Task 10: `fake-agent.sh`

**Files:**
- Create: `crates/camp/tests/fake-agent.sh` (mode 755 — commit the exec bit)

**Interfaces:**
- Consumes: `CAMP_BIN` (test-provided), `CAMP_DIR`/`CAMP_BEAD`/`CAMP_SESSION` (campd-provided, decision J), behavior env `FAKE_AGENT_OUTCOME` / `FAKE_AGENT_MILESTONE` / `FAKE_AGENT_CRASH` / `FAKE_AGENT_HOLD_DIR` / `FAKE_AGENT_TOUCH` (test-provided, inherited through campd).
- Produces: the §16 integration workhorse — speaks the whole worker contract via the camp CLI.

- [ ] **Step 1: Write the script** — `crates/camp/tests/fake-agent.sh`:

```bash
#!/usr/bin/env bash
# The fake agent (spec §16): speaks the Gas Camp worker contract via the
# camp CLI exactly as a real worker would — claim → milestones → close —
# with env-controlled outcome, timing, and crashes. campd execs this in
# place of `claude` ([dispatch].command — visible config, not a fallback);
# claude-style argv is accepted and ignored, the contract inputs arrive in
# CAMP_* env vars (plan decision J).
#
# Behavior env (all optional):
#   FAKE_AGENT_MILESTONE  emit this milestone text after claiming
#   FAKE_AGENT_CRASH      "kill" = SIGKILL yourself; any number = exit code,
#                         both BEFORE closing the bead (mid-work crash)
#   FAKE_AGENT_HOLD_DIR   after claiming, wait until $DIR/$CAMP_BEAD exists
#                         (deterministic concurrency tests)
#   FAKE_AGENT_TOUCH      write this file (relative to cwd) to prove where
#                         the worker ran (worktree tests)
#   FAKE_AGENT_OUTCOME    close outcome, default "pass"
set -euo pipefail

: "${CAMP_BIN:?fake-agent: CAMP_BIN must point at the camp binary}"
: "${CAMP_DIR:?fake-agent: CAMP_DIR must be set by campd}"
: "${CAMP_BEAD:?fake-agent: CAMP_BEAD must be set by campd}"
: "${CAMP_SESSION:?fake-agent: CAMP_SESSION must be set by campd}"

"$CAMP_BIN" claim "$CAMP_BEAD" --session "$CAMP_SESSION"

if [[ -n "${FAKE_AGENT_TOUCH:-}" ]]; then
  echo "worked in $(pwd)" > "$FAKE_AGENT_TOUCH"
fi

if [[ -n "${FAKE_AGENT_MILESTONE:-}" ]]; then
  "$CAMP_BIN" event emit "$FAKE_AGENT_MILESTONE" --bead "$CAMP_BEAD" --session "$CAMP_SESSION"
fi

if [[ -n "${FAKE_AGENT_CRASH:-}" ]]; then
  case "$FAKE_AGENT_CRASH" in
    kill) kill -KILL $$ ;;
    *) exit "$FAKE_AGENT_CRASH" ;;
  esac
fi

if [[ -n "${FAKE_AGENT_HOLD_DIR:-}" ]]; then
  # Test-harness gate, not camp machinery: camp never polls; this script is
  # the stand-in for a model thinking. Bounded (plan-review note 3): a test
  # that dies before writing the gate file must not leave this loop spinning
  # after tempdir cleanup.
  tries=0
  until [[ -e "$FAKE_AGENT_HOLD_DIR/$CAMP_BEAD" ]]; do
    sleep 0.05
    tries=$((tries + 1))
    if [ "$tries" -gt 1200 ]; then
      echo "fake-agent: hold gate never opened for $CAMP_BEAD (60s)" >&2
      exit 97
    fi
  done
fi

"$CAMP_BIN" close "$CAMP_BEAD" --outcome "${FAKE_AGENT_OUTCOME:-pass}" --reason "fake agent done"
```

- [ ] **Step 2: Make it executable and sanity-check it standalone**

```bash
chmod +x crates/camp/tests/fake-agent.sh
bash -n crates/camp/tests/fake-agent.sh && echo syntax-ok
CAMP_BIN=/bin/echo CAMP_DIR=/tmp CAMP_BEAD=b CAMP_SESSION=s crates/camp/tests/fake-agent.sh
```
Expected: `syntax-ok`, then two echo lines (`claim b --session s`, `close b --outcome pass --reason fake agent done`), exit 0.

- [ ] **Step 3: Commit**

```bash
git add crates/camp/tests/fake-agent.sh
git commit -m "test: fake agent speaking the worker contract via the camp CLI"
```

### Task 11: Integration suite — `daemon_dispatch.rs`

**Files:**
- Create: `crates/camp/tests/daemon_dispatch.rs`

**Interfaces:**
- Consumes: the camp binary (`CARGO_BIN_EXE_camp`), fake-agent.sh, everything above.
- Produces: the master plan Phase 8 test obligations, asserted end to end with no Claude.

These tests exercise real processes (campd must own SIGCHLD), so campd is always spawned as a child process, never in-thread. Test-side waiting polls the ledger — sanctioned for harnesses only.

- [ ] **Step 1: Write the harness and the first failing test** — `crates/camp/tests/daemon_dispatch.rs`:

```rust
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Phase 8 integration (master plan test obligations; spec §8.1, §8.4,
//! §12, §13.3): sling → dispatch → claim → milestone → close with the
//! full event-with-cause trail; crash → SIGCHLD → release; the
//! concurrency cap; worktree lifecycle; registry-before-exec — all driven
//! by fake-agent.sh, no Claude anywhere.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_camp");
const READY_PREFIX: &str = "campd listening on ";

fn fake_agent() -> String {
    format!("{}/tests/fake-agent.sh", env!("CARGO_MANIFEST_DIR"))
}

fn camp(root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .env_remove("CAMP_DIR")
        .arg("--camp")
        .arg(root)
        .args(args)
        .output()
        .unwrap()
}

fn camp_ok(root: &Path, args: &[&str]) -> String {
    let out = camp(root, args);
    assert!(
        out.status.success(),
        "camp {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// A camp with one rig and full dispatch config. Returns (root, rig).
fn scaffold(dir: &Path, max_workers: usize, rig_extra: &str) -> (PathBuf, PathBuf) {
    let root = dir.join(".camp");
    std::fs::create_dir_all(&root).unwrap();
    let rig = dir.join("repo");
    std::fs::create_dir_all(&rig).unwrap();
    std::fs::write(
        root.join("camp.toml"),
        format!(
            "[camp]\nname = \"t\"\n\n[[rigs]]\nname = \"gc\"\npath = \"{}\"\nprefix = \"gc\"\n{rig_extra}\n\
             [dispatch]\nmax_workers = {max_workers}\ncommand = \"{}\"\ndefault_agent = \"dev\"\n",
            rig.display(),
            fake_agent(),
        ),
    )
    .unwrap();
    write_agent(&root, "dev", "");
    // create the ledger so every verb (and campd) finds it
    camp_ok(&root, &["events", "--json"]);
    (root, rig)
}

fn write_agent(root: &Path, name: &str, front_extra: &str) {
    let agents = root.join("agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(
        agents.join(format!("{name}.md")),
        format!("---\nname: {name}\n{front_extra}---\nDo the work.\n"),
    )
    .unwrap();
}

fn events_json(root: &Path) -> Vec<serde_json::Value> {
    camp_ok(root, &["events", "--json"])
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// Test-harness wait (camp never polls; tests may). Panics with the event
/// dump on timeout so failures are diagnosable.
fn wait_until(root: &Path, what: &str, pred: impl Fn(&[serde_json::Value]) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let events = events_json(root);
        if pred(&events) {
            return;
        }
        if Instant::now() > deadline {
            panic!("timed out waiting for {what}; events: {events:#?}");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn count(events: &[serde_json::Value], kind: &str) -> usize {
    events.iter().filter(|e| e["type"] == kind).count()
}

fn seq_of(events: &[serde_json::Value], pred: impl Fn(&serde_json::Value) -> bool) -> i64 {
    events
        .iter()
        .find(|e| pred(e))
        .unwrap_or_else(|| panic!("event not found in {events:#?}"))["seq"]
        .as_i64()
        .unwrap()
}

/// campd as a real child process with fake-agent behavior env. Drop kills
/// and reaps it (workers it spawned die on their own — fake agents exit).
struct Daemon {
    child: Child,
}

impl Daemon {
    fn spawn(root: &Path, envs: &[(&str, &str)]) -> Daemon {
        let mut cmd = Command::new(BIN);
        cmd.env_remove("CAMP_DIR")
            .env("CAMP_BIN", BIN)
            .args(["daemon", "--camp"])
            .arg(root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        for (k, v) in envs {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut line = String::new();
        BufReader::new(stdout).read_line(&mut line).unwrap();
        assert!(
            line.starts_with(READY_PREFIX),
            "unexpected first line from campd: {line:?}"
        );
        Daemon { child }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Master plan: "sling → dispatch → claim → milestone → close pass with
/// the full event-with-cause trail (spec §13.3 asserted literally)".
#[test]
fn tier0_sling_runs_the_whole_contract_with_a_causal_trail() {
    let dir = tempfile::tempdir().unwrap();
    let (root, rig) = scaffold(dir.path(), 10, "");
    let _campd = Daemon::spawn(&root, &[("FAKE_AGENT_MILESTONE", "halfway there")]);

    let bead = camp_ok(&root, &["sling", "add a --json flag"]).trim().to_owned();
    assert_eq!(bead, "gc-1");

    wait_until(&root, "the full Tier-0 trail", |e| {
        count(e, "session.stopped") == 1
    });
    let events = events_json(&root);

    // The exact causal order for this bead (spec §13.3): created →
    // dispatched (session.woke, bead linked) → claimed → milestone →
    // closed pass → stopped.
    let created = seq_of(&events, |e| e["type"] == "bead.created" && e["bead"] == bead.as_str());
    let woke = seq_of(&events, |e| e["type"] == "session.woke" && e["data"]["bead"] == bead.as_str());
    let claimed = seq_of(&events, |e| e["type"] == "bead.claimed" && e["bead"] == bead.as_str());
    let milestone = seq_of(&events, |e| e["type"] == "worker.milestone" && e["bead"] == bead.as_str());
    let closed = seq_of(&events, |e| e["type"] == "bead.closed" && e["bead"] == bead.as_str());
    let stopped = seq_of(&events, |e| e["type"] == "session.stopped");
    assert!(
        created < woke && woke < claimed && claimed < milestone && milestone < closed && closed < stopped,
        "causal order violated: {events:#?}"
    );

    // Registry facts (spec §7.4): name, agent, claude session id (uuid),
    // transcript path computed from the WORKER cwd (the rig, F3).
    let woke_ev = events.iter().find(|e| e["type"] == "session.woke").unwrap();
    assert_eq!(woke_ev["data"]["name"], "t/dev/1");
    assert_eq!(woke_ev["data"]["agent"], "dev");
    let sid = woke_ev["data"]["claude_session_id"].as_str().unwrap();
    assert_eq!(sid.len(), 36, "claude_session_id must be a uuid: {sid}");
    let transcript = woke_ev["data"]["transcript_path"].as_str().unwrap();
    let munged_rig: String = rig
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    assert!(
        transcript.contains(&munged_rig) && transcript.ends_with(&format!("{sid}.jsonl")),
        "transcript {transcript} must be under the munged rig dir {munged_rig}"
    );

    // The worker's close carries the milestone actor = session name.
    let ms = events.iter().find(|e| e["type"] == "worker.milestone").unwrap();
    assert_eq!(ms["actor"], "t/dev/1");
    // stopped records exit 0 (F4)
    let st = events.iter().find(|e| e["type"] == "session.stopped").unwrap();
    assert_eq!(st["data"]["exit_code"], 0);

    // Envelope capture exists (decision G)
    assert!(root.join("sessions").join("t-dev-1.json").exists());

    // The state fold agrees with the whole story.
    let out = camp(&root, &["doctor", "--refold"]);
    assert!(out.status.success(), "refold drift after a Tier-0 run");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --package camp --test daemon_dispatch`
Expected: FAIL (before Tasks 5–9 land, compile errors; after them, this is the end-to-end proof run — it must PASS once wiring is complete).

- [ ] **Step 3: Add the remaining integration tests** (same file; each is one master-plan obligation):

```rust
/// spec §13.3's literal example shape: "gc-1 closed → gc-2 ready →
/// dispatched (session)". A dependent bead's dispatch must trail its
/// blocker's close in the ledger.
#[test]
fn a_close_unblocks_and_dispatches_the_dependent() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 10, "");
    let hold = dir.path().join("hold");
    std::fs::create_dir_all(&hold).unwrap();
    let _campd = Daemon::spawn(&root, &[("FAKE_AGENT_HOLD_DIR", hold.to_str().unwrap())]);

    let a = camp_ok(&root, &["sling", "A"]).trim().to_owned();
    wait_until(&root, "A's worker to wake", |e| count(e, "session.woke") == 1);
    // B depends on A; created while A is held mid-work.
    let out = camp_ok(&root, &["create", "B", "--needs", &a]);
    let b = out.trim().to_owned();

    // release A: it closes pass, its worker exits, B dispatches
    std::fs::write(hold.join(&a), "go").unwrap();
    std::fs::write(hold.join(&b), "go").unwrap(); // B may run to completion too
    wait_until(&root, "B's worker to wake", |e| {
        e.iter().any(|ev| ev["type"] == "session.woke" && ev["data"]["bead"] == b.as_str())
    });

    let events = events_json(&root);
    let a_closed = seq_of(&events, |e| e["type"] == "bead.closed" && e["bead"] == a.as_str());
    let b_woke = seq_of(&events, |e| e["type"] == "session.woke" && e["data"]["bead"] == b.as_str());
    assert!(
        a_closed < b_woke,
        "the trail must read: {a} closed → {b} dispatched; events: {events:#?}"
    );
}

/// Master plan: "crash mid-work → SIGCHLD → session.crashed → bead back
/// to open" — nonzero-exit variant.
#[test]
fn a_crash_mid_work_releases_the_bead() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 10, "");
    let _campd = Daemon::spawn(
        &root,
        &[("FAKE_AGENT_CRASH", "7"), ("FAKE_AGENT_MILESTONE", "about to die")],
    );

    let bead = camp_ok(&root, &["sling", "doomed"]).trim().to_owned();
    wait_until(&root, "the crash to be recorded", |e| count(e, "session.crashed") == 1);

    let events = events_json(&root);
    let crashed = events.iter().find(|e| e["type"] == "session.crashed").unwrap();
    assert_eq!(crashed["data"]["exit_code"], 7, "F4: nonzero exit is a crash");
    // the milestone proves the crash was mid-work (after claim)
    let claimed = seq_of(&events, |e| e["type"] == "bead.claimed");
    let crashed_seq = seq_of(&events, |e| e["type"] == "session.crashed");
    assert!(claimed < crashed_seq);
    // fold released the bead: open again, unclaimed, visible as ready
    let ls = camp_ok(&root, &["ls", "--ready", "--json"]);
    let rows: serde_json::Value = serde_json::from_str(&ls).unwrap();
    let row = rows.as_array().unwrap().iter().find(|r| r["id"] == bead.as_str())
        .expect("crashed bead must be open and ready again");
    assert_eq!(row["status"], "open");
    assert!(row["claimed_by"].is_null());
    // and Phase 8 deliberately does NOT respawn it (decision C):
    assert_eq!(count(&events, "session.woke"), 1);
}

/// F4's signal row, observed for real: SIGKILL ⇒ session.crashed with
/// signal 9.
#[test]
fn a_sigkilled_worker_is_a_crash_with_the_signal_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 10, "");
    let _campd = Daemon::spawn(&root, &[("FAKE_AGENT_CRASH", "kill")]);
    camp_ok(&root, &["sling", "shot"]);
    wait_until(&root, "the kill to be recorded", |e| count(e, "session.crashed") == 1);
    let events = events_json(&root);
    let crashed = events.iter().find(|e| e["type"] == "session.crashed").unwrap();
    assert_eq!(crashed["data"]["signal"], 9);
    assert!(crashed["data"].get("exit_code").is_none());
}

/// Master plan: "concurrency cap honored under a burst of ready beads
/// (11 ready, 10 spawned, 11th dispatched on first close)".
#[test]
fn the_cap_holds_at_ten_and_the_eleventh_dispatches_on_first_close() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 10, "");
    let hold = dir.path().join("hold");
    std::fs::create_dir_all(&hold).unwrap();
    let _campd = Daemon::spawn(&root, &[("FAKE_AGENT_HOLD_DIR", hold.to_str().unwrap())]);

    let beads: Vec<String> = (0..11)
        .map(|i| camp_ok(&root, &["sling", &format!("job {i}")]).trim().to_owned())
        .collect();

    // exactly 10 workers wake and claim; the 11th bead stays undispatched
    wait_until(&root, "ten claims", |e| count(e, "bead.claimed") == 10);
    let events = events_json(&root);
    assert_eq!(count(&events, "session.woke"), 10, "the cap is 10");
    let dispatched: Vec<&str> = events
        .iter()
        .filter(|e| e["type"] == "session.woke")
        .map(|e| e["data"]["bead"].as_str().unwrap())
        .collect();
    let waiting: Vec<&String> = beads.iter().filter(|b| !dispatched.contains(&b.as_str())).collect();
    assert_eq!(waiting.len(), 1, "exactly one bead must wait for capacity");
    let eleventh = waiting[0].clone();

    // first close frees capacity; the 11th dispatches
    std::fs::write(hold.join(dispatched[0]), "go").unwrap();
    wait_until(&root, "the 11th dispatch", |e| {
        e.iter().any(|ev| ev["type"] == "session.woke" && ev["data"]["bead"] == eleventh.as_str())
    });

    // drain everyone; the ledger-reconstructed concurrency never exceeded 10
    for bead in &beads {
        let _ = std::fs::write(hold.join(bead), "go");
    }
    wait_until(&root, "all workers to finish", |e| {
        count(e, "session.stopped") == 11
    });
    let events = events_json(&root);
    let mut live = 0i64;
    let mut max_live = 0i64;
    for e in &events {
        match e["type"].as_str().unwrap() {
            "session.woke" => {
                live += 1;
                max_live = max_live.max(live);
            }
            "session.stopped" | "session.crashed" => live -= 1,
            _ => {}
        }
    }
    assert_eq!(max_live, 10, "the ledger must show the cap was never exceeded");
}

fn git_rig(rig: &Path) {
    for args in [
        vec!["init", "-b", "main"],
        vec!["config", "user.email", "t@t"],
        vec!["config", "user.name", "t"],
        vec!["commit", "--allow-empty", "-m", "init"],
    ] {
        let out = Command::new("git").arg("-C").arg(rig).args(&args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }
}

/// Master plan: "worktree created/removed on pass". The worker runs in the
/// worktree (proven by FAKE_AGENT_TOUCH landing there), and a clean pass
/// reaps it with the gc-mirrored event.
#[test]
fn worktree_isolation_creates_then_reaps_on_pass() {
    let dir = tempfile::tempdir().unwrap();
    let (root, rig) = scaffold(dir.path(), 10, "");
    git_rig(&rig);
    write_agent(&root, "dev", "isolation: worktree\n");
    let hold = dir.path().join("hold");
    std::fs::create_dir_all(&hold).unwrap();
    let _campd = Daemon::spawn(
        &root,
        &[
            ("FAKE_AGENT_HOLD_DIR", hold.to_str().unwrap()),
            ("FAKE_AGENT_TOUCH", "proof.txt"),
        ],
    );

    let bead = camp_ok(&root, &["sling", "isolated work"]).trim().to_owned();
    wait_until(&root, "the isolated worker to claim", |e| count(e, "bead.claimed") == 1);

    let wt = root.join("worktrees").join(&bead);
    assert!(wt.join(".git").exists(), "worktree must exist mid-run at {}", wt.display());
    assert!(wt.join("proof.txt").exists(), "the worker's cwd must be the worktree");
    // registry records it (decision E)
    let events = events_json(&root);
    let woke = events.iter().find(|e| e["type"] == "session.woke").unwrap();
    assert_eq!(woke["data"]["worktree"], wt.to_str().unwrap());

    std::fs::write(hold.join(&bead), "go").unwrap();
    wait_until(&root, "the worktree reap", |e| count(e, "bead.worktree.reaped") == 1);
    assert!(!wt.exists(), "a passed bead's worktree is removed (spec §12)");
    let events = events_json(&root);
    let reaped = events.iter().find(|e| e["type"] == "bead.worktree.reaped").unwrap();
    assert_eq!(reaped["bead"], bead.as_str());
    assert_eq!(reaped["data"]["path"], wt.to_str().unwrap());
}

/// Master plan: "worktree kept on fail" — with the reason in the event.
#[test]
fn worktree_is_kept_with_an_event_on_fail() {
    let dir = tempfile::tempdir().unwrap();
    let (root, rig) = scaffold(dir.path(), 10, "");
    git_rig(&rig);
    write_agent(&root, "dev", "isolation: worktree\n");
    let _campd = Daemon::spawn(&root, &[("FAKE_AGENT_OUTCOME", "fail")]);

    let bead = camp_ok(&root, &["sling", "will fail"]).trim().to_owned();
    wait_until(&root, "the kept worktree", |e| count(e, "worktree.kept") == 1);

    let wt = root.join("worktrees").join(&bead);
    assert!(wt.exists(), "a failed bead's worktree is kept for forensics (spec §12)");
    let events = events_json(&root);
    let kept = events.iter().find(|e| e["type"] == "worktree.kept").unwrap();
    assert_eq!(kept["bead"], bead.as_str());
    assert!(
        kept["data"]["reason"].as_str().unwrap().contains("did not close pass"),
        "kept: {kept}"
    );
}

/// Master plan: "registry row precedes process start" — observed via a
/// spawn that cannot succeed: the woke row (with claude session id and
/// transcript path) commits, then the failure lands as session.crashed
/// with the reason. Nothing dangles.
#[test]
fn the_registry_row_precedes_the_process_and_spawn_failures_land_in_the_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 10, "");
    // break the worker command AFTER scaffold wrote it
    let toml = std::fs::read_to_string(root.join("camp.toml")).unwrap();
    std::fs::write(
        root.join("camp.toml"),
        toml.replace(&fake_agent(), "/nonexistent/no-such-worker"),
    )
    .unwrap();
    let _campd = Daemon::spawn(&root, &[]);

    camp_ok(&root, &["sling", "never runs"]);
    wait_until(&root, "the spawn failure", |e| count(e, "session.crashed") == 1);

    let events = events_json(&root);
    let woke = seq_of(&events, |e| e["type"] == "session.woke");
    let crashed = seq_of(&events, |e| e["type"] == "session.crashed");
    assert!(woke < crashed, "registry at birth: woke commits before the exec attempt");
    let woke_ev = events.iter().find(|e| e["type"] == "session.woke").unwrap();
    assert_eq!(woke_ev["data"]["claude_session_id"].as_str().unwrap().len(), 36);
    assert!(woke_ev["data"]["transcript_path"].as_str().unwrap().ends_with(".jsonl"));
    let crashed_ev = events.iter().find(|e| e["type"] == "session.crashed").unwrap();
    assert!(
        crashed_ev["data"]["reason"].as_str().unwrap().contains("spawn failed"),
        "crashed: {crashed_ev}"
    );
}

/// Routing (decision D) through the daemon: the rig's default_agent
/// outranks [dispatch].default_agent; session names carry the agent.
#[test]
fn rig_default_agent_routes_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 10, "default_agent = \"rigger\"\n");
    write_agent(&root, "rigger", "");
    let _campd = Daemon::spawn(&root, &[]);
    camp_ok(&root, &["sling", "routed"]);
    wait_until(&root, "the routed worker", |e| count(e, "session.stopped") == 1);
    let events = events_json(&root);
    let woke = events.iter().find(|e| e["type"] == "session.woke").unwrap();
    assert_eq!(woke["data"]["agent"], "rigger");
    assert_eq!(woke["data"]["name"], "t/rigger/1");
}

/// A cooked-formula-shaped bead with no assignee and no routable default
/// lands dispatch.failed in the ledger (decision F) — campd's errors are
/// events, and campd survives.
#[test]
fn an_unroutable_bead_lands_dispatch_failed_and_campd_survives() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 10, "");
    // remove the default agent from [dispatch]
    let toml = std::fs::read_to_string(root.join("camp.toml")).unwrap();
    std::fs::write(root.join("camp.toml"), toml.replace("default_agent = \"dev\"\n", "")).unwrap();
    let _campd = Daemon::spawn(&root, &[]);

    // create (not sling — sling validates routing client-side)
    let bead = camp_ok(&root, &["create", "orphan work"]).trim().to_owned();
    wait_until(&root, "the dispatch failure", |e| count(e, "dispatch.failed") == 1);
    let events = events_json(&root);
    let failed = events.iter().find(|e| e["type"] == "dispatch.failed").unwrap();
    assert_eq!(failed["bead"], bead.as_str());
    assert!(failed["data"]["reason"].as_str().unwrap().contains("default_agent"));
    // exactly once per bead per campd lifetime (decision F): a second
    // unroutable bead fails once; the first does NOT re-fail on its poke
    camp_ok(&root, &["create", "another"]);
    wait_until(&root, "the second bead's dispatch failure", |e| {
        count(e, "dispatch.failed") == 2
    });
    // a further poke re-fails neither bead
    camp_ok(&root, &["event", "emit", "poke"]);
    camp_ok(&root, &["top"]); // campd still answers (and this settles the poke)
    assert_eq!(
        count(&events_json(&root), "dispatch.failed"),
        2,
        "one per unroutable bead, not per poke"
    );
}
```

Note on the last assertion: TWO `dispatch.failed` events are expected — one per unroutable bead ("orphan work" and "another"), each exactly once. If executing this reveals an off-by-one in the guard semantics, the guard (decision F: per-bead, per-lifetime) is the contract; fix the code, not the contract.

- [ ] **Step 4: Run the whole suite**

Run: `cargo test --package camp --test daemon_dispatch -- --test-threads=4`
Expected: PASS, every test. These are real-process tests; expect ~20–60 s total. If any test times out, read the panic's event dump — the trail names the first missing link.

- [ ] **Step 5: Run the full workspace gates**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace`
Expected: all green — including Phase 7's daemon_lifecycle (unchanged behavior for camps with no dispatchable work) and camp-core's refold property.

- [ ] **Step 6: Commit**

```bash
git add crates/camp/tests/daemon_dispatch.rs
git commit -m "test: Phase 8 integration - Tier-0 trail, crash, cap, worktrees, registry-at-birth"
```


### Task 12: Spec §7.1 layout line (decision G)

**Files:**
- Modify: `docs/design/2026-07-05-gas-camp-design.md` (§7.1 layout block, one added line)

The spec-and-code-never-diverge rule requires the `sessions/` capture directory to appear in the layout the spec documents, in the same PR that creates it.

- [ ] **Step 1: Edit the layout block.** In the §7.1 code block, after the `camp.db` lines and before `runs/<run-id>/`, add:

```
  sessions/              # per-worker stdout capture (the claude result
                         #   envelope JSON) + stderr log, one pair per session
```

- [ ] **Step 2: Verify the diff touches only that block**

Run: `git diff docs/design/2026-07-05-gas-camp-design.md`
Expected: one hunk, two added lines, nothing else.

- [ ] **Step 3: Commit**

```bash
git add docs/design/2026-07-05-gas-camp-design.md
git commit -m "docs: spec §7.1 — sessions/ worker capture directory (Phase 8)"
```

### Task 13: Gates, PR, report

- [ ] **Step 1: Rebase onto current main** (mandatory before any push; siblings phase-10-orders and phase-6-gc-compat-ci may have merged):

```bash
git fetch origin && git rebase origin/main
```
Resolve conflicts additively (shared files: `main.rs`, `event.rs`, `vocab.rs`, `fold.rs`, `config.rs`, `Cargo.toml`s, `Cargo.lock`, and especially `daemon/event_loop.rs` — see decision I's token layout and the Phase 10 settle seam). Re-run all gates after any rebase.

- [ ] **Step 2: Full gates**

```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace
```
Expected: all green. Also grep-verify no polling slipped in:

```bash
grep -rn "sleep\|interval\|tick" crates/camp/src/ crates/camp-core/src/
```
Expected: no hits in camp code (the fake agent's `sleep 0.05` hold-gate and test helpers live under `tests/`, which is out of scope for this grep).

- [ ] **Step 3: Push and open the PR**

```bash
git push -u origin phase-8-dispatch-workers
gh pr create --title "Phase 8: dispatch and workers" --body "$(cat <<'EOF'
Spec §8.1/§8.4/§12; master plan Phase 8. Plan: docs/superpowers/plans/2026-07-07-phase-8-dispatch-workers.md

- pack.rs: Claude Code agent files, last-wins layering (packs then <camp>/agents/)
- [dispatch] config (max_workers/command/default_agent) + per-rig default_agent
- Dispatcher: query-driven converge to cap on every wake; registry-at-birth (F1);
  spawn argv pinned to F1-F7 (golden test); stdin /dev/null (F5); transcript path
  from worker cwd (F3); SIGCHLD self-pipe -> try_wait reap -> F4 exit mapping
- Worktree isolation: created on dispatch, reaped on pass (bead.worktree.reaped),
  kept with reason on anything else (worktree.kept)
- camp sling (Tier 0, auto-start + routing fail-fast), camp event emit
- New events: worker.milestone, worktree.kept (camp), bead.worktree.reaped
  (gc-mirrored), dispatch.failed (camp; plan decision F — flagged addition)
- fake-agent.sh + daemon_dispatch.rs: Tier-0 causal trail (§13.3), crash release,
  11/10 cap with ledger-reconstructed max concurrency, worktree lifecycle,
  registry-precedes-exec, unroutable-bead dispatch.failed
- Spec §7.1: sessions/ capture dir added in this PR (decision G)

Token layout coordinated with phase-10-orders: 0 listener, 1 config watch (P10),
2 SIGCHLD (P8), 3+ connections.
EOF
)"
gh pr checks --watch
```
Expected: CI green on all jobs. Not complete until it is.

- [ ] **Step 4: Report to the lead** — PR number, CI status, and each master-plan exit criterion quoted with its evidence (see the map below).

## Exit-Criteria Evidence Map

| Master plan Phase 8 exit criterion (verbatim) | Evidence |
|---|---|
| "Tier-0 path complete and evented end to end with the fake agent" | `tier0_sling_runs_the_whole_contract_with_a_causal_trail` (created→woke→claimed→milestone→closed→stopped, refold clean); `a_close_unblocks_and_dispatches_the_dependent` (§13.3 literal trail) |
| "real-`claude` spawn arguments match Phase 2's pinned facts" | `argv_matches_the_fixture_facts_for_a_fully_pinned_agent` + `undeclared_agent_fields_emit_no_flags` (F1/F2/F7, decision L), `transcript_path_munges_every_non_alphanumeric_to_dash` (F3), `classify_maps_exits_per_f4` + crash/SIGKILL integration tests (F4), `Stdio::null()` in `spawn::spawn` (F5), `--resume` untouched-by-design (F6 — resume is the Phase 11 nudge/attach surface; no id churn: ids are pre-assigned and stable) |
| "CI green" | `gh pr checks --watch` output in the report |

Test-obligation cross-check (master plan "Tests" paragraph): sling→dispatch→claim→milestone→close ✓ (tier0 test); event-with-cause trail §13.3 ✓ (tier0 + dependent tests); crash → SIGCHLD → session.crashed → bead open ✓ (crash + SIGKILL tests); concurrency cap 11/10/first-close ✓ (cap test, incl. ledger-reconstructed max-live == 10); worktree created/removed on pass, kept on fail ✓ (two worktree tests); registry row precedes process start ✓ (spawn-failure ordering test).

## Self-Review Notes (writing-plans skill)

- **Spec coverage:** §8.1 sling/routing (Tasks 6, 8, 11), §8.4 headless-but-present + worker contract + non-interactive permissions surface (Tasks 7, 8; permission pinning is pack content per F7/decision L), §12 cwd/worktrees (Tasks 7, 8, 11), §7.3 dispatch-on-write with no query loops (decision B; converge only on wakes), §7.4 registry-at-birth (Tasks 8, 11), §10.1 death detection (Tasks 8, 9, 11), §13.2/§13.3 guarantees (Task 11 assertions). Master-plan interfaces (`AgentDef`, `resolve_agent`, `[dispatch]` keys, session name shape, event names) match the pinned contract verbatim; `resolve_agent(cfg, name)` keeps its pinned two-argument signature via decision Q.
- **Deliberate scope boundaries (not gaps):** respawn-after-crash → Phase 9/11 (decision C); envelope parsing → Phase 9+ (decision G); pid in registry → Phase 11 seam (decision E); stream-json stdin workers → Phase 11 (K); attended-teammate sling surface → Phase 12 (master plan); `sling --formula` → Phase 9 cook wiring.
- **Type consistency check:** `Dispatcher::new(CampDir, CampConfig)`, `converge(&mut Ledger)`, `reap(&mut Ledger)` consistent across Tasks 8/9/11; `build_spec` 8-arg signature consistent between Tasks 7/8; `dispatchable_beads`/`next_session_name` consistent between Tasks 3/8; event names consistent with Task 2 everywhere.
- **Known execution-time watch items:** (1) yaml-rust2 indexing API details in Task 4 — if `doc[key]` on a non-hash panics internally, switch to `doc.as_hash()` lookups; the tests pin behavior, not the API. (2) The clippy `zombie_processes` allow in `spawn::spawn` — justified in-code (SIGCHLD-driven try_wait reaps; workers deliberately outlive a killed campd for adoption). (3) `daemon_with_a_broken_config_refuses_to_start` runs `run()` in-process — it returns the config error before binding, so no thread/kill gymnastics are needed.

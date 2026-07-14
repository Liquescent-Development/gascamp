//! The dispatcher (spec §7.3, §8.3, §8.4): on every wake, converge the
//! ledger's dispatchable set onto live worker children, up to
//! [dispatch].max_workers. Query-driven from ledger truth (Phase 8 plan
//! decision B) — crash-only, no in-memory queue to lose. Every failure
//! lands in the ledger (`dispatch.failed`, `session.crashed`), never in a
//! void: campd has no caller (invariant 5).

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::ExitStatus;

use anyhow::{Context as _, Result};
use camp_core::Seq;
use camp_core::config::CampConfig;
use camp_core::event::{EventInput, EventType};
use camp_core::ledger::Ledger;
use camp_core::pack::{self, Isolation};
use camp_core::readiness::BeadRow;

use super::bounded::{self, STDIN_WRITE_TIMEOUT};
use super::spawn::{self, SpawnSpec};
use crate::campdir::CampDir;

pub struct Dispatcher {
    camp: CampDir,
    config: CampConfig,
    /// Live children by pid. campd is the parent (spec §10.1) — SIGCHLD
    /// lands here and try_wait reaps.
    children: HashMap<u32, Worker>,
    /// Auxiliary children (patrol nudge-resume spawns, Phase 11): reaped
    /// in the same try_wait sweep; failures land as patrol.degraded, never
    /// as session events.
    aux: HashMap<u32, AuxChild>,
    /// Patrol respawns deferred because the worker cap was full (Phase 11,
    /// round-2 LOW 2): retried on every `converge`, so a reap-freed slot
    /// re-hooks the bead — never stranded, never silently dropped
    /// (invariant 3). Insertion order preserved so the oldest deferral
    /// re-hooks first. (Phase 9's retry machinery will subsume budget and
    /// backoff for these — the hand-off seam.)
    pending_respawns: Vec<String>,
}

struct Worker {
    child: std::process::Child,
    session: String,
    bead: String,
    rig: String,
    rig_path: PathBuf,
    worktree: Option<PathBuf>,
    /// The session-end event committed (PR #14 review finding 1): a
    /// disposition retry must skip the end-append or the fold's
    /// already-ended rejection would wedge the pid forever.
    end_recorded: bool,
    /// The held stream-json stdin (Decision C). Dropping it is the release
    /// EOF; `None` after release (or for Null-mode spawns). A mio pipe
    /// Sender rather than a raw ChildStdin so nudge writes can be BOUNDED
    /// (non-blocking + waitable, PR #51 review finding 2) — an unbounded
    /// blocking write into a full pipe would wedge campd's single-threaded
    /// event loop.
    stdin: Option<mio::unix::pipe::Sender>,
    /// Set when campd released this worker (bead closed, stdin dropped):
    /// its exit reaps as session.stopped with this reason — campd
    /// initiated the termination of a worker whose work was done (C2).
    released: Option<String>,
    /// Set when patrol killed this worker: its exit reaps as
    /// session.crashed carrying this cause_seq (the agent.stalled event).
    patrol_kill: Option<Seq>,
    /// cp-0 (§2.3): a custom kill reason set by `kill_worker_with_reason`
    /// (e.g. "stream cap exceeded max_stream_bytes"). Overrides the default
    /// "patrol restart" reason in the reap classification so the ledger
    /// names the cap (invariant 3: the ledger tells the whole story).
    kill_reason: Option<String>,
}

struct AuxChild {
    child: std::process::Child,
    session: String,
    purpose: String,
}

/// How a nudge write went (Phase 11 plan Task 11.9).
#[derive(Debug)]
pub enum NudgeOutcome {
    /// The status-request turn is in the worker's stdin pipe.
    Delivered,
    /// No held pipe for that session (released, Null-mode, or not our
    /// child) — the caller falls back to the resume path.
    NoPipe,
    /// The pipe is broken: evented as nudge_failed by the caller.
    Failed(String),
}

/// cp-1 (§2): how a control-message write went.
///
/// `NoPipe` is a caller-visible FAILURE here, NOT a designed degrade — and that
/// is the difference from `NudgeOutcome`. A turn has a resume path
/// (`claude --resume`); an interrupt does not. A worker campd holds no pipe to
/// simply CANNOT be interrupted, and answering `{"ok":true}` to that would be a
/// silent no-op dressed as success.
#[derive(Debug)]
pub enum ControlWrite {
    /// The control line is in the worker's stdin pipe.
    Delivered,
    /// campd holds no stdin pipe for that session.
    NoPipe,
    /// The write was ATTEMPTED and FAILED — so bytes may already have reached
    /// the pipe, and it has been torn down. The caller must be loud in BOTH
    /// channels: an error to the operator AND a durable fault (§2.1).
    Failed(String),
}

/// A reap failure, typed for the caller's retry decision (PR #14 fix-pass
/// NEW MEDIUM): ledger failures are retry-worthy (SQLite contention is
/// transient and bounded by busy_timeout); a try_wait failure is an OS
/// error that no retry storm will fix — self-raising on it would hot-spin.
#[derive(Debug)]
pub struct ReapFailure {
    pub retryable: bool,
    pub error: anyhow::Error,
}

impl std::fmt::Display for ReapFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#}", self.error)
    }
}

/// Everything prepare() resolves before any side effect.
struct Prep {
    spec: SpawnSpec,
    agent_name: String,
    rig_path: PathBuf,
    make_worktree: bool,
    /// The rig's base commit at dispatch time (None: non-repo/unborn HEAD)
    /// — recorded in session.woke; the shipped gate's descent reference.
    base: Option<String>,
    /// The F7 pins as spawned (model, permission_mode, comma-joined
    /// allowedTools) — recorded in session.woke; re-applied on resume.
    pins: (Option<String>, Option<String>, Option<String>),
    /// `[dispatch] exec_timeout` resolved once per dispatch (issue #55):
    /// the bound on every git subprocess launch() runs on the loop.
    exec_timeout: std::time::Duration,
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
            aux: HashMap::new(),
            pending_respawns: Vec::new(),
        }
    }

    /// Swap the routing config on a hot reload (issue #28). Only future
    /// routing decisions see it — `route`, `pack::resolve_agent`, the rig
    /// lookup, and the `max_workers` cap all read `self.config` on the next
    /// `converge`. In-flight children are untouched: each carries its own
    /// already-resolved spec, so a reload never disturbs running work.
    pub fn apply_config(&mut self, config: CampConfig) {
        self.config = config;
    }

    /// Whether campd holds this session as a live child of its own.
    pub fn is_child(&self, session: &str) -> bool {
        self.children.values().any(|w| w.session == session)
    }

    /// (rig, bead) of a live child by session — the nudge handler's
    /// session.nudged enrichment (dispatch-lifecycle Phase 1); None when
    /// the session is not our child.
    pub fn child_info(&self, session: &str) -> Option<(String, String)> {
        self.children
            .values()
            .find(|w| w.session == session)
            .map(|w| (w.rig.clone(), w.bead.clone()))
    }

    /// Write one status-request turn into the session's held stdin
    /// (Decision C: the live nudge path). The write is BOUNDED (PR #51
    /// review finding 2): Request::Nudge made this operator-triggerable
    /// over the socket, and an unbounded blocking write into the full pipe
    /// of a worker that stopped reading would wedge campd's single-threaded
    /// event loop — no dispatch, no SIGCHLD reaping — until it drained. On
    /// a bounded failure the pipe may hold a torn partial line, so it is
    /// dropped: no later turn can interleave garbage, and the worker sees
    /// EOF after draining (the release shape).
    pub fn nudge_via_stdin(&mut self, session: &str, text: &str) -> NudgeOutcome {
        let Some(worker) = self.children.values_mut().find(|w| w.session == session) else {
            return NudgeOutcome::NoPipe;
        };
        let Some(stdin) = worker.stdin.as_mut() else {
            return NudgeOutcome::NoPipe;
        };
        let line = spawn::user_message(text);
        match bounded::write_bounded(stdin, line.as_bytes(), STDIN_WRITE_TIMEOUT) {
            Ok(()) => NudgeOutcome::Delivered,
            Err(e) => {
                worker.stdin = None; // torn pipe: never write after a failed line
                NudgeOutcome::Failed(format!("stdin write failed: {e}"))
            }
        }
    }

    /// cp-1 (§2): write ONE control line into the session's held stdin — the
    /// same pipe a turn goes down, because campd already holds it and building
    /// a second transport to the same process would be a second thing to get
    /// wrong.
    ///
    /// BOUNDED, for the same reason `nudge_via_stdin` is (PR #51 finding 2):
    /// `session.interrupt` is operator-triggerable over the socket, and an
    /// unbounded blocking write into the full pipe of a worker that has stopped
    /// reading would wedge campd's single-threaded event loop — no dispatch, no
    /// SIGCHLD reaping — until it drained. That is issue #55's wedge class.
    ///
    /// On failure the pipe may hold a TORN PARTIAL LINE, so it is dropped: no
    /// later turn or control message may interleave garbage behind it. The
    /// worker sees EOF after draining (the release shape).
    pub fn write_control(&mut self, session: &str, line: &str) -> ControlWrite {
        let Some(worker) = self.children.values_mut().find(|w| w.session == session) else {
            return ControlWrite::NoPipe;
        };
        let Some(stdin) = worker.stdin.as_mut() else {
            return ControlWrite::NoPipe;
        };
        match bounded::write_bounded(stdin, line.as_bytes(), STDIN_WRITE_TIMEOUT) {
            Ok(()) => ControlWrite::Delivered,
            Err(e) => {
                worker.stdin = None; // torn pipe: never write after a failed line
                ControlWrite::Failed(format!("stdin control write failed: {e}"))
            }
        }
    }

    /// Patrol restart, child half: SIGKILL our own worker and mark the
    /// cause. The SIGCHLD reap then appends the caused session.crashed,
    /// the fold releases the bead, and converge respawns — each step its
    /// own event. Returns false when the session is not our child (the
    /// AdoptedPid path handles those).
    pub fn kill_worker(&mut self, session: &str, cause_seq: Seq) -> bool {
        let Some(worker) = self.children.values_mut().find(|w| w.session == session) else {
            return false;
        };
        worker.patrol_kill = Some(cause_seq);
        worker.stdin = None; // no more turns for a condemned worker
        if let Err(e) = worker.child.kill() {
            // Already exiting: the reap classifies it with the marked
            // cause regardless.
            eprintln!("campd: patrol kill of {session}: {e}");
        }
        true
    }

    /// cp-0 (§2.3): kill a worker with a custom reason (the max_stream_bytes
    /// ceiling). The reap appends `session.crashed` carrying this reason
    /// and `cause_seq` (the `session.stream_capped` event's seq), so the
    /// ledger names the cap. Otherwise identical to `kill_worker`.
    ///
    /// Returns `Ok(false)` when NO live child holds that session (an adopted
    /// worker from a previous campd life, or one already reaped) — the
    /// caller MUST handle that: a `session.stream_capped` with no kill
    /// behind it is a campd action with no ledger consequence (invariant 3).
    ///
    /// review fix 3: a failing `child.kill()` is now PROPAGATED, not
    /// swallowed into an `eprintln!` that then returned `true` — stderr is
    /// neither the caller nor the ledger (invariant 5).
    pub fn kill_worker_with_reason(
        &mut self,
        session: &str,
        cause_seq: Seq,
        reason: String,
    ) -> Result<bool> {
        let Some(worker) = self.children.values_mut().find(|w| w.session == session) else {
            return Ok(false);
        };
        worker.patrol_kill = Some(cause_seq);
        worker.kill_reason = Some(reason);
        worker.stdin = None; // no more turns for a condemned worker
        worker
            .child
            .kill()
            .with_context(|| format!("cap-breach kill of {session}"))?;
        Ok(true)
    }

    /// The release rule (Decision C2): the bead closed, so drop the held
    /// stdin (EOF) and mark the worker released — its exit reaps as
    /// session.stopped with the reason. Returns the session name when a
    /// live un-released worker held that bead (the caller arms the release
    /// grace timer); None otherwise (idempotent).
    pub fn release_worker(&mut self, bead: &str, reason: &str) -> Option<String> {
        let worker = self
            .children
            .values_mut()
            .find(|w| w.bead == bead && w.released.is_none())?;
        worker.stdin = None;
        worker.released = Some(reason.to_owned());
        Some(worker.session.clone())
    }

    /// The release grace expired and the worker is still ours: terminate
    /// it (P3: an idle stream worker does not exit on EOF alone).
    pub fn kill_released(&mut self, session: &str) -> bool {
        let Some(worker) = self
            .children
            .values_mut()
            .find(|w| w.session == session && w.released.is_some())
        else {
            return false;
        };
        if let Err(e) = worker.child.kill() {
            eprintln!("campd: release kill of {session}: {e}");
        }
        true
    }

    /// Spawn an auxiliary patrol child (nudge-resume). Reaped in the
    /// normal SIGCHLD sweep; a nonzero exit lands as patrol.degraded.
    pub fn spawn_aux(
        &mut self,
        session: &str,
        purpose: &str,
        mut cmd: std::process::Command,
    ) -> Result<()> {
        let child = cmd.spawn().with_context(|| format!("spawning {purpose}"))?;
        self.aux.insert(
            child.id(),
            AuxChild {
                child,
                session: session.to_owned(),
                purpose: purpose.to_owned(),
            },
        );
        Ok(())
    }

    /// Test observability: whether every aux child has exited.
    #[cfg(test)]
    pub fn aux_done(&mut self) -> bool {
        self.aux
            .values_mut()
            .all(|a| matches!(a.child.try_wait(), Ok(Some(_))))
    }

    /// Every pid campd owns (workers + aux children): the adoption/restart
    /// probe excludes these — a live nudge-resume child carries the
    /// worker's session uuid in its argv and must never be mistaken for
    /// the worker itself (Phase 11 plan Task 11.11/11.12).
    pub fn known_pids(&self) -> std::collections::HashSet<u32> {
        self.children
            .keys()
            .chain(self.aux.keys())
            .copied()
            .collect()
    }

    /// Test scaffolding (patrol's executor tests live in a sibling
    /// module): a live held-stdin `cat` worker registered under the given
    /// session/bead, stdout captured to `<dir>/<bead>.out`.
    #[cfg(test)]
    #[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    pub(crate) fn test_insert_held_cat(
        &mut self,
        dir: &std::path::Path,
        session: &str,
        bead: &str,
    ) -> u32 {
        let _spawning = crate::daemon::spawn_probe_guard();
        let out = std::fs::File::create(dir.join(format!("{bead}.out"))).unwrap_or_else(|e| {
            panic!("creating the capture file: {e}");
        });
        let mut child = std::process::Command::new("cat")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::from(out))
            .spawn()
            .unwrap_or_else(|e| panic!("spawning cat: {e}"));
        let stdin = child.stdin.take().map(mio::unix::pipe::Sender::from);
        let pid = child.id();
        self.children.insert(
            pid,
            Worker {
                child,
                session: session.to_owned(),
                bead: bead.to_owned(),
                rig: "gc".into(),
                rig_path: dir.to_path_buf(),
                worktree: None,
                end_recorded: false,
                stdin,
                released: None,
                patrol_kill: None,
                kill_reason: None,
            },
        );
        pid
    }

    /// Test scaffolding (review finding 2, PR #51): a live held-stdin
    /// worker that NEVER reads its pipe (`sleep`), so writes into it fill
    /// the OS buffer and stall — the wedge shape the bounded nudge write
    /// must survive.
    #[cfg(test)]
    #[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    pub(crate) fn test_insert_held_sleeper(
        &mut self,
        dir: &std::path::Path,
        session: &str,
        bead: &str,
    ) -> u32 {
        let _spawning = crate::daemon::spawn_probe_guard();
        let mut child = std::process::Command::new("sleep")
            .arg("60")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap_or_else(|e| panic!("spawning sleep: {e}"));
        let stdin = child.stdin.take().map(mio::unix::pipe::Sender::from);
        let pid = child.id();
        self.children.insert(
            pid,
            Worker {
                child,
                session: session.to_owned(),
                bead: bead.to_owned(),
                rig: "gc".into(),
                rig_path: dir.to_path_buf(),
                worktree: None,
                end_recorded: false,
                stdin,
                released: None,
                patrol_kill: None,
                kill_reason: None,
            },
        );
        pid
    }

    /// Test scaffolding: kill a worker child and reap the OS process (no
    /// ledger effects — cleanup only).
    #[cfg(test)]
    #[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    pub(crate) fn test_kill_and_wait(&mut self, pid: u32) {
        let worker = self
            .children
            .get_mut(&pid)
            .unwrap_or_else(|| panic!("no worker at pid {pid}"));
        let _ = worker.child.kill();
        let _ = worker.child.wait();
    }

    /// Test scaffolding: block until the given worker child exits.
    #[cfg(test)]
    #[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    pub(crate) fn test_child_wait(&mut self, pid: u32) -> std::process::ExitStatus {
        self.children
            .get_mut(&pid)
            .unwrap_or_else(|| panic!("no worker at pid {pid}"))
            .child
            .wait()
            .unwrap_or_else(|e| panic!("waiting on {pid}: {e}"))
    }

    /// Dispatch until the cap or the well runs dry. Re-queries after every
    /// spawn: the just-committed session.woke removes the bead from the
    /// dispatchable set, so the ledger is the only bookkeeping. Deferred
    /// patrol respawns (round-2 LOW 2) get first crack at freed slots —
    /// they represent in-flight work patrol is recovering. Dispatch failures
    /// suppress re-dispatch through the ledger's `dispatch_failure` marker
    /// (invariant 3), not an in-memory set — a marked bead leaves
    /// `dispatchable_beads`, so the loop advances and never hot-loops the
    /// same failure.
    pub fn converge(&mut self, ledger: &mut Ledger) -> Result<()> {
        self.retry_pending_respawns(ledger)?;
        loop {
            if self.children.len() >= self.config.dispatch.max_workers {
                return Ok(());
            }
            let Some(bead) = ledger.dispatchable_beads()?.into_iter().next() else {
                return Ok(());
            };
            self.dispatch_one(ledger, &bead)?;
        }
    }

    /// Re-attempt every cap-deferred patrol respawn (round-2 LOW 2). Runs
    /// at the top of `converge`, so a reap-freed slot re-hooks the oldest
    /// deferral first; a still-full cap re-queues it (idempotent, no
    /// duplicate event). Bounded by the queue, which only shrinks here.
    fn retry_pending_respawns(&mut self, ledger: &mut Ledger) -> Result<()> {
        for bead_id in std::mem::take(&mut self.pending_respawns) {
            self.dispatch_bead(ledger, &bead_id)?;
        }
        Ok(())
    }

    /// Targeted respawn for a patrol restart (Phase 11, spec §10.2): the
    /// general dispatchable set deliberately excludes ever-sessioned beads
    /// (Phase 8 decision C — organic crashes must not hot-loop until the
    /// Phase 9 retry machinery routes them); a PATROL-caused crash is
    /// budget-bounded by the ladder, so its bead re-hooks through this
    /// explicit path instead. A cap-full dispatcher QUEUES the respawn and
    /// events the deferral once — retried on the next `converge` when a
    /// slot frees, never stranded (round-2 LOW 2; invariants 3/5).
    pub fn dispatch_bead(&mut self, ledger: &mut Ledger, bead_id: &str) -> Result<()> {
        let Some(bead) = ledger.get_bead(bead_id)? else {
            return Ok(()); // gone from the ledger: nothing to re-hook
        };
        if bead.status != "open" {
            return Ok(()); // closed or re-claimed since the kill
        }
        if self.children.len() >= self.config.dispatch.max_workers {
            // Queue for retry on the next freed slot. Event ONCE per
            // deferral episode — a retry that is still capped re-queues
            // silently (no dispatch.failed spam). The reason carries the
            // shared DEFERRED_DISPATCH_PREFIX (issue #83 review F1): this
            // is campd's OWN pending retry, so the `stuck` count, `camp
            // show`'s hint, and `camp retry` all recognize and exclude it.
            if !self.pending_respawns.iter().any(|b| b == bead_id) {
                self.pending_respawns.push(bead_id.to_owned());
                ledger.append(EventInput {
                    kind: EventType::DispatchFailed,
                    rig: Some(bead.rig.clone()),
                    actor: "campd".into(),
                    bead: Some(bead.id.clone()),
                    data: serde_json::json!({
                        "reason": format!(
                            "{} worker cap reached; will retry when a slot frees",
                            camp_core::readiness::DEFERRED_DISPATCH_PREFIX
                        ),
                    }),
                })?;
            }
            return Ok(());
        }
        self.dispatch_one(ledger, &bead)
    }

    /// One bead → one worker. Per-bead failures append dispatch.failed and
    /// return Ok — a broken bead must not stall its neighbors; a ledger
    /// failure is the only Err.
    fn dispatch_one(&mut self, ledger: &mut Ledger, bead: &BeadRow) -> Result<()> {
        let prep = match self.prepare(ledger, bead) {
            Ok(prep) => prep,
            Err(reason) => {
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
    /// (the one side-effectful step) is created in launch() so nothing
    /// needs undoing on earlier failures. Err is a reason string for
    /// dispatch.failed.
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
        let exec_timeout = self
            .config
            .dispatch
            .exec_timeout()
            .map_err(|e| e.to_string())?;
        // A hung/unrunnable git is a dispatch failure (issue #55), never
        // a silent "no base" — Ok(None) is reserved for an observed
        // non-repo/unborn HEAD.
        let base =
            spawn::rig_base(&rig.path, exec_timeout).map_err(|e| format!("rig base: {e:#}"))?;
        let pins = (
            agent.model.clone(),
            agent.permission_mode.clone(),
            agent.tools.as_ref().map(|t| t.join(",")),
        );
        let session_name = ledger
            .next_session_name(&self.config.camp.name, &agent.name)
            .map_err(|e| format!("session name allocation failed: {e}"))?;
        let session_id = spawn::new_session_id();
        let make_worktree = agent.isolation == Isolation::Worktree;
        // Canonicalize the worker cwd ONCE (Phase 15 e2e finding). Real claude
        // resolves its cwd via realpath before computing the transcript project
        // dir (F3), so campd must too — otherwise the registry records, and
        // patrol watches (spec §10), a path claude never writes whenever the
        // rig/camp path contains a symlink component (e.g. macOS /var ->
        // /private/var, or a symlinked repo). Fail fast: a raw-path fallback
        // would reintroduce the bug. The worktree lifecycle (ensure_worktree /
        // reap, below) stays in raw terms — same inode via the symlink — while
        // the worker cwd + transcript use the canonical form claude will see.
        let cwd = if make_worktree {
            // The worktree leaf is created later (ensure_worktree) and does not
            // exist yet, so canonicalize the camp root (always present) and
            // append the plain worktrees/<bead> tail.
            let canon_root = std::fs::canonicalize(&self.camp.root)
                .map_err(|e| format!("canonicalize camp root {}: {e}", self.camp.root.display()))?;
            canon_root.join("worktrees").join(&bead.id)
        } else {
            std::fs::canonicalize(&rig.path)
                .map_err(|e| format!("canonicalize rig cwd {}: {e}", rig.path.display()))?
        };
        let claude_root = spawn::claude_config_root().map_err(|e| format!("{e:#}"))?;
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
            // Decision C: ALL campd dispatch spawns hold the stream stdin
            // (the live nudge path; fake agents tolerate it, C3). NOT
            // command-sniffed — a mode fallback would be a hidden branch.
            spawn::StdinMode::HeldStream,
        );
        Ok(Prep {
            spec,
            agent_name: agent.name,
            rig_path: rig.path.clone(),
            make_worktree,
            base,
            pins,
            exec_timeout,
        })
    }

    /// Registry at birth, then exec (F1). A spawn failure after the woke
    /// row committed appends session.crashed with the reason — the row
    /// must never dangle live (plan decision F).
    fn launch(&mut self, ledger: &mut Ledger, bead: &BeadRow, prep: Prep) -> Result<()> {
        let worktree = if prep.make_worktree {
            // ensure_worktree (Phase 11 Decision H): a patrol respawn
            // reuses the bead's own worktree; residue still fails fast.
            match spawn::ensure_worktree(
                &prep.rig_path,
                &self.camp.worktrees_path(),
                &bead.id,
                prep.exec_timeout,
            ) {
                Ok(dir) => Some(dir),
                Err(e) => {
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
            // The explicit live-tree opt-out (spec §12, dispatch-lifecycle
            // Q1): make it LOUD — a ledger fact before the registry row,
            // never a silent default (invariant 3).
            ledger.append(EventInput {
                kind: EventType::DispatchLiveTree,
                rig: Some(bead.rig.clone()),
                actor: "campd".into(),
                bead: Some(bead.id.clone()),
                data: serde_json::json!({
                    "path": prep.spec.cwd,
                    "agent": prep.agent_name,
                }),
            })?;
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
        // Phase 3: the dispatch-time base (the shipped gate's descent
        // reference) and the F7 pins (re-applied on resume turns) ride the
        // woke JSON like `worktree` — no sessions-table schema change.
        if let Some(base) = &prep.base {
            woke["base"] = serde_json::json!(base);
        }
        let (model, permission_mode, allowed_tools) = &prep.pins;
        if let Some(m) = model {
            woke["model"] = serde_json::json!(m);
        }
        if let Some(p) = permission_mode {
            woke["permission_mode"] = serde_json::json!(p);
        }
        if let Some(t) = allowed_tools {
            woke["allowed_tools"] = serde_json::json!(t);
        }
        ledger.append(EventInput {
            kind: EventType::SessionWoke,
            rig: Some(bead.rig.clone()),
            actor: "campd".into(),
            bead: Some(bead.id.clone()),
            data: woke,
        })?;

        match spawn::spawn(&prep.spec) {
            Ok(mut child) => {
                // HeldStream: the task is the FIRST user_message on the
                // held pipe (P2). A write failure means the worker never
                // got its task — kill it and land the failure in the
                // ledger like any other spawn failure (decision F).
                let mut stdin = child.stdin.take().map(mio::unix::pipe::Sender::from);
                if let Some(pipe) = stdin.as_mut() {
                    let task = spawn::task_message(&bead.id, &prep.spec.session_name);
                    // BOUNDED like every write into a held pipe (issue
                    // #55; the PR #51 finding 2 discipline). The pipe is
                    // fresh, so a task that fits the OS buffer succeeds on
                    // the first write — the deadline only ever fires on a
                    // task larger than the buffer fed to a worker that
                    // never started reading, which previously wedged the
                    // whole event loop.
                    if let Err(e) =
                        bounded::write_bounded(pipe, task.as_bytes(), STDIN_WRITE_TIMEOUT)
                    {
                        let _ = child.kill();
                        let _ = child.wait();
                        ledger.append(EventInput {
                            kind: EventType::SessionCrashed,
                            rig: Some(bead.rig.clone()),
                            actor: "campd".into(),
                            bead: None,
                            data: serde_json::json!({
                                "name": prep.spec.session_name,
                                "reason": format!("task write failed: {e}"),
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
                                    "reason": "task write failed before the worker ran",
                                }),
                            })?;
                        }
                        return Ok(());
                    }
                }
                self.children.insert(
                    child.id(),
                    Worker {
                        child,
                        session: prep.spec.session_name,
                        bead: bead.id.clone(),
                        rig: bead.rig.clone(),
                        rig_path: prep.rig_path,
                        worktree,
                        end_recorded: false,
                        stdin,
                        released: None,
                        patrol_kill: None,
                        kill_reason: None,
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

    /// SIGCHLD service (plan decision I). Durable-then-forget (PR #14
    /// review finding 1): each ledger effect commits before the next step,
    /// and the worker leaves the map only after BOTH the session end and
    /// the worktree disposition landed. A failed append surfaces and the
    /// next wake retries — try_wait re-returns the exit status, and
    /// `end_recorded` makes the retry skip the already-committed session
    /// end so the fold never sees a double end.
    /// Reap auxiliary patrol children (nudge-resume spawns): exit 0 is
    /// forgotten; a nonzero exit lands as patrol.degraded naming the
    /// session — never a session event (the aux child is not the worker).
    /// Durable-then-forget like the worker half: a failed append keeps the
    /// aux entry so the next wake retries.
    fn reap_aux(&mut self, ledger: &mut Ledger) -> Result<(), ReapFailure> {
        let mut exited: Vec<(u32, ExitStatus)> = Vec::new();
        for (pid, aux) in &mut self.aux {
            match aux.child.try_wait() {
                Ok(Some(status)) => exited.push((*pid, status)),
                Ok(None) => {}
                Err(e) => {
                    return Err(ReapFailure {
                        retryable: false,
                        error: anyhow::Error::new(e).context("try_wait on an aux child"),
                    });
                }
            }
        }
        for (pid, status) in exited {
            let Some(aux) = self.aux.get(&pid) else {
                continue;
            };
            if !status.success() {
                ledger
                    .append(EventInput {
                        kind: EventType::PatrolDegraded,
                        rig: None,
                        actor: "campd".into(),
                        bead: None,
                        data: serde_json::json!({
                            "error": format!("{} child exited {status}", aux.purpose),
                            "session": aux.session,
                        }),
                    })
                    .map_err(|e| ReapFailure {
                        retryable: true,
                        error: anyhow::Error::new(e).context("recording an aux failure"),
                    })?;
            }
            self.aux.remove(&pid);
        }
        Ok(())
    }

    pub fn reap(&mut self, ledger: &mut Ledger) -> Result<(), ReapFailure> {
        // The worktree-removal bound (issue #55): resolved before the
        // sweep; validated at config load, so an Err here is a hand-built
        // config — surfaced, not defaulted (invariant 5), and no retry
        // storm can fix it.
        let exec_timeout = self
            .config
            .dispatch
            .exec_timeout()
            .map_err(|e| ReapFailure {
                retryable: false,
                error: anyhow::Error::new(e).context("resolving [dispatch] exec_timeout"),
            })?;
        self.reap_aux(ledger)?;
        let mut exited: Vec<(u32, ExitStatus)> = Vec::new();
        for (pid, worker) in &mut self.children {
            match worker.child.try_wait() {
                Ok(Some(status)) => exited.push((*pid, status)),
                Ok(None) => {}
                Err(e) => {
                    // An OS-level failure, not contention: retrying in a
                    // tight loop cannot help (NEW MEDIUM).
                    return Err(ReapFailure {
                        retryable: false,
                        error: anyhow::Error::new(e).context("try_wait on a worker"),
                    });
                }
            }
        }
        for (pid, status) in exited {
            let Some(worker) = self.children.get_mut(&pid) else {
                continue;
            };
            if !worker.end_recorded {
                // Phase 11 classification order: a campd-initiated end
                // overrides F4 — released ⇒ stopped-with-reason (C2: the
                // work was done; the exit code is noise), patrol kill ⇒
                // crashed-with-cause (the agent.stalled seq). Otherwise
                // the plain F4 mapping.
                let (kind, exit_code, signal) = classify(status);
                let mut data = serde_json::json!({ "name": worker.session });
                if let Some(code) = exit_code {
                    data["exit_code"] = serde_json::json!(code);
                }
                if let Some(sig) = signal {
                    data["signal"] = serde_json::json!(sig);
                }
                let kind = if let Some(reason) = &worker.released {
                    data["reason"] = serde_json::json!(reason);
                    EventType::SessionStopped
                } else if let Some(cause_seq) = worker.patrol_kill {
                    let reason = worker
                        .kill_reason
                        .clone()
                        .unwrap_or_else(|| "patrol restart".to_owned());
                    data["reason"] = serde_json::json!(reason);
                    data["cause_seq"] = serde_json::json!(cause_seq);
                    EventType::SessionCrashed
                } else {
                    kind
                };
                ledger
                    .append(EventInput {
                        kind,
                        rig: Some(worker.rig.clone()),
                        actor: "campd".into(),
                        bead: None,
                        data,
                    })
                    .map_err(|e| ReapFailure {
                        retryable: true,
                        error: anyhow::Error::new(e).context("recording a session end"),
                    })?;
                worker.end_recorded = true;
            }
            if let Some(worker) = self.children.get(&pid) {
                Self::dispose_worktree(ledger, worker, exec_timeout).map_err(|error| {
                    ReapFailure {
                        retryable: true,
                        error,
                    }
                })?;
            }
            // Every ledger effect landed; now it is safe to forget.
            self.children.remove(&pid);
        }
        Ok(())
    }

    /// Decision H: closed-pass ⇒ remove + bead.worktree.reaped (gc name);
    /// anything else ⇒ keep + worktree.kept with the reason. A failed
    /// removal keeps, with the git error as the reason — never silent.
    /// Idempotent for retries (finding 1): a closed-pass worktree that is
    /// already gone was removed by a previous attempt whose event append
    /// failed — record the reap rather than re-removing.
    fn dispose_worktree(
        ledger: &mut Ledger,
        worker: &Worker,
        exec_timeout: std::time::Duration,
    ) -> Result<()> {
        let Some(worktree) = &worker.worktree else {
            return Ok(());
        };
        let closed_pass = ledger
            .get_bead(&worker.bead)?
            .is_some_and(|b| b.status == "closed" && b.outcome.as_deref() == Some("pass"));
        let (kind, data) = if closed_pass {
            let removal = if worktree.exists() {
                spawn::remove_worktree(&worker.rig_path, worktree, exec_timeout)
            } else {
                Ok(()) // already removed by a prior attempt
            };
            match removal {
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
                    "reason": format!(
                        "bead {} did not close pass; kept for forensics",
                        worker.bead
                    ),
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

// ---------------------------------------------------------------------------
// Phase 9: the graph runtime (spec §8.3) — campd as purely mechanical
// control dispatcher for check loops, retry classification, on_complete
// fan-out, and run finalization. Ledger bookkeeping runs INSIDE the cursor
// transaction (CampdProcessor -> GraphRuntime::process -> Ledger::append_on),
// so every action is exactly-once across kill -9; only check-script spawns
// and bond cooks are side effects, queued here and drained by
// event_loop::settle (the Phase 10 pending-cook pattern).
//
// Every append is guarded by a state precondition the append destroys
// (claim requires open; attempt creation requires budget; anchor close
// requires not-closed; finalization requires the root open), so the settle
// fixpoint does bounded work per invocation.
//
// Zero-Framework-Cognition: this code counts attempts, walks declared
// edges, and applies declared budgets. Judgments come from workers (close
// outcomes/classifications) and user-supplied check scripts.
// ---------------------------------------------------------------------------

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use camp_core::error::CoreError;
use camp_core::event::Event;
use camp_core::formula::ast::Step;
use camp_core::formula::runtime::{self as flow, RunContext, RunVerdict};
use rusqlite::Connection;

/// A check-script run the settle loop owes (Task 6 executes it).
#[derive(Debug, Clone, PartialEq)]
pub struct PendingCheck {
    pub run_id: String,
    pub step_id: String,
    pub anchor: String,
    pub attempt_bead: String,
    /// 1-based: this is the Nth check run of the step.
    pub attempt_no: u32,
}

/// A fan-out the settle loop owes (Task 7 executes it): cook the missing
/// bond children of a closed-pass on_complete anchor.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingFanout {
    pub run_id: String,
    pub step_id: String,
    pub anchor: String,
}

pub struct GraphRuntime {
    camp_root: PathBuf,
    /// rig snapshot (check-script cwd, bond-cook prefix) — same
    /// campd-start freshness as the Dispatcher's config.
    rigs: HashMap<String, camp_core::config::RigConfig>,
    /// Run-context cache; `None` = the run dir failed to load and the run
    /// was dead-ended (evented once, never retried silently).
    runs: HashMap<String, Option<Arc<RunContext>>>,
    pending_checks: Vec<PendingCheck>,
    pending_fanouts: Vec<PendingFanout>,
    /// Live check-script children by pid, reaped on SIGCHLD like workers.
    check_children: HashMap<u32, CheckChild>,
}

/// One running check script (spec §8.3): campd runs `check.path` with
/// cwd = rig, `min(check.timeout, step.timeout)` enforced via the poll
/// timeout, output captured to a per-attempt log under the run dir.
struct CheckChild {
    child: std::process::Child,
    run_id: String,
    step_id: String,
    anchor: String,
    attempt_bead: String,
    attempt_no: u32,
    deadline: Option<std::time::Instant>,
    log_path: PathBuf,
    timed_out: bool,
}

/// The rig snapshot GraphRuntime keeps (check-script cwd, bond-cook
/// prefix), built from a config's rigs. One source of truth for `new` and
/// `apply_config` (issue #28 hot reload).
fn rig_snapshot(
    config: &camp_core::config::CampConfig,
) -> HashMap<String, camp_core::config::RigConfig> {
    config
        .rigs
        .iter()
        .map(|r| (r.name.clone(), r.clone()))
        .collect()
}

impl GraphRuntime {
    pub fn new(camp_root: PathBuf, config: &camp_core::config::CampConfig) -> GraphRuntime {
        GraphRuntime {
            camp_root,
            rigs: rig_snapshot(config),
            runs: HashMap::new(),
            pending_checks: Vec::new(),
            pending_fanouts: Vec::new(),
            check_children: HashMap::new(),
        }
    }

    /// Refresh the rig snapshot on a hot reload (issue #28), so check-script
    /// cwd and bond-cook prefixes follow the same config the dispatcher and
    /// order scheduler run. In-flight check children keep the cwd they were
    /// spawned with; only future spawns see the new rigs.
    pub fn apply_config(&mut self, config: &camp_core::config::CampConfig) {
        self.rigs = rig_snapshot(config);
    }

    /// The cursor-atomic hook: called from CampdProcessor::process for
    /// every committed event, inside the cursor transaction. All appends
    /// go through Ledger::append_on on `conn`.
    pub fn process(
        &mut self,
        conn: &Connection,
        now: &str,
        event: &Event,
    ) -> Result<(), CoreError> {
        match event.kind {
            EventType::RunCooked => self.on_run_cooked(conn, now, event),
            EventType::BeadCreated => self.on_bead_created(conn, now, event),
            EventType::BeadClosed => self.on_bead_closed(conn, now, event),
            _ => Ok(()),
        }
    }

    /// The side-effect executor, called from event_loop::settle between
    /// orders::settle and dispatcher.converge: spawn due check scripts
    /// (verdicts arrive via SIGCHLD -> reap_checks) and cook due bond
    /// children (Task 7). On an infrastructure error the unexecuted work
    /// is requeued and the error surfaces — the next settle retries (the
    /// Phase 10 pending-cook pattern).
    pub fn execute(&mut self, ledger: &mut Ledger) -> Result<()> {
        let pending = std::mem::take(&mut self.pending_checks);
        for (i, check) in pending.iter().enumerate() {
            let live = self.check_children.values().any(|c| {
                c.run_id == check.run_id
                    && c.step_id == check.step_id
                    && c.attempt_no == check.attempt_no
            });
            if live {
                continue; // already running (defensive dedupe)
            }
            if let Err(error) = self.spawn_check(ledger, check) {
                for survivor in &pending[i..] {
                    self.pending_checks.push(survivor.clone());
                }
                return Err(error);
            }
        }
        let fanouts = std::mem::take(&mut self.pending_fanouts);
        for (i, fanout) in fanouts.iter().enumerate() {
            if let Err(error) = self.execute_fanout(ledger, fanout) {
                for survivor in &fanouts[i..] {
                    self.pending_fanouts.push(survivor.clone());
                }
                return Err(error);
            }
        }
        Ok(())
    }

    /// Cook the bond children a closed-pass on_complete anchor is owed
    /// (spec §8.2): parallel cooks every missing item; sequential cooks
    /// item N only when children 0..N all closed pass (lazy chaining,
    /// plan Decision 4 — a non-pass child halts by cooking nothing).
    /// Fan-out-level failures (bad for_each/vars, missing bond formula,
    /// cook errors) append dispatch.failed on the anchor and drop the
    /// fan-out — evented, never silent, never fatal (Ok). Only ledger
    /// errors surface as Err.
    fn execute_fanout(&mut self, ledger: &mut Ledger, fanout: &PendingFanout) -> Result<()> {
        let Some(ctx) = self.ctx(&fanout.run_id) else {
            return Ok(());
        };
        let Some(step_ref) = ctx.step_ref(&fanout.step_id) else {
            return Ok(());
        };
        let Some(oc) = &step_ref.step.on_complete else {
            return Ok(());
        };
        let Some(close_data) = ledger.close_event_data(&fanout.anchor)? else {
            return Ok(()); // anchor not closed yet: nothing due
        };
        if close_data["outcome"] != "pass" {
            return Ok(());
        }
        let items = match flow::resolve_for_each(&close_data, &oc.for_each) {
            Ok(items) => items.clone(),
            Err(reason) => return fanout_failure(ledger, fanout, &ctx, &reason),
        };
        let children = existing_bond_children(ledger, &fanout.anchor)?;
        let due: Vec<usize> = if oc.parallel {
            (0..items.len())
                .filter(|i| !children.contains_key(i))
                .collect()
        } else {
            // sequential: the next index, only when every existing child
            // closed pass (a non-pass child halts the chain)
            let next = children.len();
            let all_passed = children
                .values()
                .all(|row| row.status == "closed" && row.outcome.as_deref() == Some("pass"));
            if all_passed && next < items.len() && !children.contains_key(&next) {
                vec![next]
            } else {
                Vec::new()
            }
        };
        if due.is_empty() {
            return Ok(());
        }
        let Some(rig) = self.rigs.get(&ctx.rig).cloned() else {
            return fanout_failure(
                ledger,
                fanout,
                &ctx,
                &format!("rig {:?} is not configured", ctx.rig),
            );
        };
        let bond_path = self
            .camp_root
            .join("formulas")
            .join(format!("{}.toml", oc.bond));
        let bond = match camp_core::formula::parse_and_validate(&bond_path) {
            Ok(bond) => bond,
            Err(e) => {
                return fanout_failure(
                    ledger,
                    fanout,
                    &ctx,
                    &format!("bond formula {:?} is unusable: {e}", oc.bond),
                );
            }
        };
        for index in due {
            let item = &items[index];
            let vars = match flow::substitute_vars(&oc.vars, item, index) {
                Ok(vars) => vars,
                Err(reason) => return fanout_failure(ledger, fanout, &ctx, &reason),
            };
            let extra_root_needs = if !oc.parallel && index > 0 {
                // the literal chain edge (plan Decision 4): audit + gate
                match children.get(&(index - 1)) {
                    Some(prev) => vec![prev.id.clone()],
                    None => Vec::new(),
                }
            } else {
                Vec::new()
            };
            let opts = camp_core::formula::CookOptions {
                vars,
                extra_root_needs,
                extra_root_labels: vec![flow::bond_label(&fanout.anchor, index)],
            };
            if let Err(e) = camp_core::formula::cook_with(
                ledger,
                &bond,
                &flow::runs_dir(&self.camp_root),
                &rig,
                "campd",
                &opts,
            ) {
                return fanout_failure(
                    ledger,
                    fanout,
                    &ctx,
                    &format!("cooking bond {:?} item {index} failed: {e}", oc.bond),
                );
            }
        }
        Ok(())
    }

    /// Spawn one check script (non-blocking; the SIGCHLD reap classifies).
    /// A script that cannot START is structural, not flaky: the anchor
    /// hard-fails immediately without burning the budget (plan Decision
    /// 10). Only ledger errors surface as Err.
    fn spawn_check(&mut self, ledger: &mut Ledger, pending: &PendingCheck) -> Result<()> {
        let Some(ctx) = self.ctx(&pending.run_id) else {
            return Ok(()); // dead-ended elsewhere
        };
        let Some(step_ref) = ctx.step_ref(&pending.step_id) else {
            return Ok(());
        };
        let Some(check) = &step_ref.step.check else {
            return Ok(());
        };
        // the anchor must still be campd's loop (idempotent re-execution)
        let Some(anchor_row) = ledger.get_bead(&pending.anchor)? else {
            return Ok(());
        };
        if anchor_row.status != "in_progress" || anchor_row.claimed_by.as_deref() != Some("campd") {
            return Ok(());
        }
        let Some(rig_path) = self.rigs.get(&ctx.rig).map(|r| r.path.clone()) else {
            return self.check_spawn_failure(
                ledger,
                pending,
                &anchor_row,
                &format!("rig {:?} is not configured", ctx.rig),
            );
        };
        let script = if check.path.is_absolute() {
            check.path.clone()
        } else {
            rig_path.join(&check.path)
        };
        let log_dir = flow::runs_dir(&self.camp_root)
            .join(&pending.run_id)
            .join("checks");
        if let Err(e) = std::fs::create_dir_all(&log_dir) {
            return self.check_spawn_failure(
                ledger,
                pending,
                &anchor_row,
                &format!("cannot create {}: {e}", log_dir.display()),
            );
        }
        let log_path = log_dir.join(format!(
            "{}-attempt-{}.log",
            pending.step_id, pending.attempt_no
        ));
        let open_log = || {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
        };
        let (stdout, stderr) = match (open_log(), open_log()) {
            (Ok(a), Ok(b)) => (a, b),
            (Err(e), _) | (_, Err(e)) => {
                return self.check_spawn_failure(
                    ledger,
                    pending,
                    &anchor_row,
                    &format!("cannot open {}: {e}", log_path.display()),
                );
            }
        };
        let deadline = match (check.timeout, step_ref.step.timeout) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        }
        .map(|d| std::time::Instant::now() + d);
        let spawned = std::process::Command::new(&script)
            .current_dir(&rig_path)
            .env("CAMP_DIR", &self.camp_root)
            .env("CAMP_BEAD", &pending.anchor)
            .env("CAMP_RUN_ID", &pending.run_id)
            .env("CAMP_STEP_ID", &pending.step_id)
            .env("CAMP_ATTEMPT", pending.attempt_no.to_string())
            .stdin(std::process::Stdio::null())
            .stdout(stdout)
            .stderr(stderr)
            .spawn();
        match spawned {
            Ok(child) => {
                self.check_children.insert(
                    child.id(),
                    CheckChild {
                        child,
                        run_id: pending.run_id.clone(),
                        step_id: pending.step_id.clone(),
                        anchor: pending.anchor.clone(),
                        attempt_bead: pending.attempt_bead.clone(),
                        attempt_no: pending.attempt_no,
                        deadline,
                        log_path,
                        timed_out: false,
                    },
                );
                Ok(())
            }
            Err(e) => self.check_spawn_failure(
                ledger,
                pending,
                &anchor_row,
                &format!("check script {} failed to start: {e}", script.display()),
            ),
        }
    }

    /// Decision 10: a check that cannot start closes the anchor hard,
    /// evidence in one atomic batch — no budget loop over a structural
    /// problem.
    fn check_spawn_failure(
        &mut self,
        ledger: &mut Ledger,
        pending: &PendingCheck,
        anchor_row: &BeadRow,
        error: &str,
    ) -> Result<()> {
        ledger.append_batch(vec![
            EventInput {
                kind: EventType::CheckFailed,
                rig: Some(anchor_row.rig.clone()),
                actor: "campd".into(),
                bead: Some(pending.attempt_bead.clone()),
                data: serde_json::json!({
                    "run_id": pending.run_id,
                    "step_id": pending.step_id,
                    "attempt": pending.attempt_no,
                    "error": error,
                }),
            },
            EventInput {
                kind: EventType::BeadClosed,
                rig: Some(anchor_row.rig.clone()),
                actor: "campd".into(),
                bead: Some(anchor_row.id.clone()),
                data: serde_json::json!({
                    "outcome": "fail",
                    "final_disposition": "hard_fail",
                    "reason": error,
                }),
            },
        ])?;
        Ok(())
    }

    /// The earliest live check deadline as a poll timeout contribution
    /// (monotonic; combined with the cron heap via min_deadline —
    /// Decision 11c).
    pub fn poll_timeout(&self, now: std::time::Instant) -> Option<Duration> {
        self.check_children
            .values()
            .filter(|c| !c.timed_out)
            .filter_map(|c| c.deadline)
            .min()
            .map(|deadline| deadline.saturating_duration_since(now))
    }

    /// Kill checks past their declared deadline (SIGKILL; the reap
    /// classifies them as timed-out failed iterations — Decision 12).
    /// `now` is a parameter for testability.
    pub fn kill_expired(&mut self, now: std::time::Instant) {
        for check in self.check_children.values_mut() {
            if check.timed_out {
                continue;
            }
            if let Some(deadline) = check.deadline
                && deadline <= now
            {
                // Review MEDIUM 1: a child that ALREADY EXITED met its
                // deadline — the wake being late (busy settle) must not
                // rewrite an on-time verdict. Only a still-running child
                // is timed out and killed; the reap classifies exited
                // ones by their real status.
                if matches!(check.child.try_wait(), Ok(Some(_))) {
                    continue;
                }
                check.timed_out = true;
                let _ = check.child.kill();
            }
        }
    }

    /// Reap exited check scripts (SIGCHLD service, alongside the worker
    /// reap): each verdict is ONE atomic batch. Durable-then-forget: a
    /// failed batch keeps the child mapped and surfaces retryable; the
    /// next wake retries (try_wait re-returns the cached status).
    pub fn reap_checks(&mut self, ledger: &mut Ledger) -> Result<(), ReapFailure> {
        let mut exited: Vec<(u32, ExitStatus)> = Vec::new();
        for (pid, check) in &mut self.check_children {
            match check.child.try_wait() {
                Ok(Some(status)) => exited.push((*pid, status)),
                Ok(None) => {}
                Err(e) => {
                    return Err(ReapFailure {
                        retryable: false,
                        error: anyhow::Error::new(e).context("try_wait on a check script"),
                    });
                }
            }
        }
        for (pid, status) in exited {
            let Some(check) = self.check_children.get(&pid) else {
                continue;
            };
            let verdict =
                self.check_verdict(ledger, check, status)
                    .map_err(|error| ReapFailure {
                        retryable: true,
                        error: anyhow::anyhow!("{error}").context("recording a check verdict"),
                    })?;
            if let Some(inputs) = verdict {
                ledger.append_batch(inputs).map_err(|error| ReapFailure {
                    retryable: true,
                    error: anyhow::Error::new(error).context("recording a check verdict"),
                })?;
            }
            self.check_children.remove(&pid);
        }
        Ok(())
    }

    /// Build the verdict batch for one exited check (pure over ledger
    /// reads; None = the anchor is no longer campd's — drop silently, the
    /// ledger already tells that story).
    fn check_verdict(
        &self,
        ledger: &Ledger,
        check: &CheckChild,
        status: ExitStatus,
    ) -> Result<Option<Vec<EventInput>>, CoreError> {
        use std::os::unix::process::ExitStatusExt;
        let Some(ctx) = self.runs.get(&check.run_id).and_then(Clone::clone) else {
            return Ok(None);
        };
        let Some(step_ref) = ctx.step_ref(&check.step_id) else {
            return Ok(None);
        };
        let Some(max_attempts) = step_ref.step.check.as_ref().map(|c| c.max_attempts) else {
            return Ok(None);
        };
        let Some(anchor_row) = ledger.get_bead(&check.anchor)? else {
            return Ok(None);
        };
        if anchor_row.status != "in_progress" || anchor_row.claimed_by.as_deref() != Some("campd") {
            return Ok(None);
        }
        let passed = status.success() && !check.timed_out;
        if passed {
            let output = ledger
                .close_event_data(&check.attempt_bead)?
                .and_then(|d| d.get("output").cloned());
            let mut close = serde_json::json!({
                "outcome": "pass",
                "reason": format!("check passed on attempt {}", check.attempt_no),
            });
            if let Some(output) = output {
                close["output"] = output;
            }
            return Ok(Some(vec![
                EventInput {
                    kind: EventType::CheckPassed,
                    rig: Some(anchor_row.rig.clone()),
                    actor: "campd".into(),
                    bead: Some(check.attempt_bead.clone()),
                    data: serde_json::json!({
                        "run_id": check.run_id,
                        "step_id": check.step_id,
                        "attempt": check.attempt_no,
                    }),
                },
                EventInput {
                    kind: EventType::BeadClosed,
                    rig: Some(anchor_row.rig.clone()),
                    actor: "campd".into(),
                    bead: Some(check.anchor.clone()),
                    data: close,
                },
            ]));
        }
        // failed (exit code, signal, or timeout): one check iteration spent
        let mut failed_data = serde_json::json!({
            "run_id": check.run_id,
            "step_id": check.step_id,
            "attempt": check.attempt_no,
            "log": check.log_path.display().to_string(),
        });
        if check.timed_out {
            failed_data["timed_out"] = serde_json::json!(true);
        }
        if let Some(code) = status.code() {
            failed_data["exit_code"] = serde_json::json!(code);
        } else if let Some(signal) = status.signal() {
            failed_data["signal"] = serde_json::json!(signal);
        }
        let failed = EventInput {
            kind: EventType::CheckFailed,
            rig: Some(anchor_row.rig.clone()),
            actor: "campd".into(),
            bead: Some(check.attempt_bead.clone()),
            data: failed_data,
        };
        if check.attempt_no < max_attempts {
            // next iteration bead: the worker redoes the work with the
            // check evidence in front of it (mechanical copying)
            let evidence = format!(
                "check failed (attempt {}): {}; log: {}\n{}",
                check.attempt_no,
                if check.timed_out {
                    "timed out".to_owned()
                } else {
                    format!("exit {:?}", status.code().unwrap_or(-1))
                },
                check.log_path.display(),
                log_tail(&check.log_path),
            );
            let base_description = ledger
                .created_event_data(&anchor_row.id)?
                .as_ref()
                .and_then(|d| d.get("description"))
                .and_then(|d| d.as_str())
                .unwrap_or_default()
                .to_owned();
            let id = ledger.next_bead_id(prefix_of(&anchor_row.id)?)?;
            let attempts_so_far = ledger
                .step_attempts(&check.run_id, &check.step_id, &anchor_row.id)?
                .len();
            let created = attempt_bead_input(
                id,
                &anchor_row.rig,
                &check.run_id,
                step_ref.step,
                &anchor_row.title,
                &base_description,
                attempts_so_far + 1,
                Some(&evidence),
            );
            return Ok(Some(vec![failed, created]));
        }
        Ok(Some(vec![
            failed,
            EventInput {
                kind: EventType::BeadClosed,
                rig: Some(anchor_row.rig.clone()),
                actor: "campd".into(),
                bead: Some(check.anchor.clone()),
                data: serde_json::json!({
                    "outcome": "fail",
                    "final_disposition": "hard_fail",
                    "reason": format!("check budget ({max_attempts}) exhausted"),
                }),
            },
        ]))
    }

    /// Startup reconciliation (kill -9 self-heals; observation over
    /// state, mirroring `unresponded_fires`): re-derive the pending work
    /// whose side effects were lost with the process.
    ///
    /// Orphan run dirs: a kill -9 between cook's run-dir write and its
    /// ledger batch leaves `runs/<id>/` with no ledger record; the
    /// fire-dedupe re-cooks under a NEW id, so recovery is idempotent but
    /// the orphan lingers — the crash window cook.rs's header records as
    /// the safe direction, its sweep deferred to a future `camp doctor`
    /// check (review note 4; nothing to reconcile here).
    pub fn reconcile(&mut self, ledger: &mut Ledger) -> Result<(), CoreError> {
        // (1) checks due / (2) defensive attempt respawns: every anchor
        // campd holds is a looping step mid-loop; its latest attempt's
        // state says what is owed (any landed verdict either closed the
        // anchor or created a newer attempt — both visible state).
        let held = ledger.list_beads(&camp_core::readiness::ListFilter {
            rig: None,
            mine: Some("campd"),
        })?;
        for anchor_row in held {
            if anchor_row.status != "in_progress" {
                continue;
            }
            let Some(membership) = ledger.run_membership(&anchor_row.id)? else {
                continue;
            };
            let Some(step_id) = membership.step_id else {
                continue;
            };
            let Some(ctx) = self.ctx(&membership.run_id) else {
                continue; // dead-ended when its events processed
            };
            let Some(step_ref) = ctx.step_ref(&step_id) else {
                continue;
            };
            if step_ref.anchor != anchor_row.id {
                continue;
            }
            let attempts = ledger.step_attempts(&membership.run_id, &step_id, &anchor_row.id)?;
            let Some(latest) = attempts.last() else {
                continue; // claim landed, attempt creation is cursor-atomic
            };
            if step_ref.step.check.is_some()
                && latest.status == "closed"
                && latest.outcome.as_deref() == Some("pass")
            {
                // the crash window: attempt passed, no verdict yet — the
                // interrupted check re-runs (checks are re-runnable)
                self.queue_check(PendingCheck {
                    run_id: membership.run_id.clone(),
                    step_id: step_id.clone(),
                    anchor: anchor_row.id.clone(),
                    attempt_bead: latest.id.clone(),
                    attempt_no: flow::check_runs_used(&attempts),
                });
            }
            if let Some(retry) = &step_ref.step.retry
                && latest.status == "closed"
                && latest.outcome.as_deref() == Some("fail")
            {
                // impossible-by-construction (cursor-atomic), but a
                // hand-edited ledger heals: recreate the missing respawn
                let used = ledger.transient_fails_used(&attempts)?;
                let latest_transient = ledger
                    .close_event_data(&latest.id)?
                    .as_ref()
                    .and_then(|d| d.get("failure_class"))
                    .and_then(|c| c.as_str())
                    == Some("transient");
                if latest_transient && used < retry.max_attempts {
                    let id = ledger.next_bead_id(prefix_of(&anchor_row.id)?)?;
                    let base_description = ledger
                        .created_event_data(&anchor_row.id)?
                        .as_ref()
                        .and_then(|d| d.get("description"))
                        .and_then(|d| d.as_str())
                        .unwrap_or_default()
                        .to_owned();
                    let input = attempt_bead_input(
                        id,
                        &anchor_row.rig,
                        &membership.run_id,
                        step_ref.step,
                        &anchor_row.title,
                        &base_description,
                        attempts.len() + 1,
                        Some("previous attempt failed transient (reconciled after restart)"),
                    );
                    ledger.append(input)?;
                }
            }
        }
        // (3) fan-outs due: every closed-pass on_complete anchor whose
        // children may be incomplete. Bounded by total run count
        // (startup-only); execute() computes what is actually owed.
        for event in ledger.events_of_type(EventType::RunCooked)? {
            let Some(run_id) = event.data["run_id"].as_str() else {
                continue;
            };
            let run_id = run_id.to_owned();
            let Some(ctx) = self.ctx(&run_id) else {
                // Review LOW 3: a dir that vanished while campd was down
                // gets no further events to trigger the processor's
                // dead-end — reconcile must event it, never skip silently.
                let inputs = ledger.dead_end_inputs(
                    &run_id,
                    event.seq,
                    "run dir unreadable; the run cannot advance",
                )?;
                if !inputs.is_empty() {
                    ledger.append_batch(inputs)?;
                }
                continue;
            };
            let ctx = Arc::clone(&ctx);
            for step in &ctx.formula.steps {
                if step.on_complete.is_none() {
                    continue;
                }
                let anchor = &ctx.anchors[&step.id];
                let Some(row) = ledger.get_bead(anchor)? else {
                    continue;
                };
                if row.status == "closed" && row.outcome.as_deref() == Some("pass") {
                    self.queue_fanout(PendingFanout {
                        run_id: run_id.clone(),
                        step_id: step.id.clone(),
                        anchor: anchor.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    fn ctx(&mut self, run_id: &str) -> Option<Arc<RunContext>> {
        if let Some(cached) = self.runs.get(run_id) {
            return cached.clone();
        }
        let loaded = match flow::load_run(&flow::runs_dir(&self.camp_root), run_id) {
            Ok(ctx) => Some(Arc::new(ctx)),
            Err(e) => {
                // surfaced here AND dead-ended durably by the caller
                eprintln!("campd: run {run_id} context load failed: {e}");
                None
            }
        };
        self.runs.insert(run_id.to_owned(), loaded.clone());
        loaded
    }

    fn on_run_cooked(
        &mut self,
        conn: &Connection,
        now: &str,
        event: &Event,
    ) -> Result<(), CoreError> {
        let run_id = event.data["run_id"].as_str().unwrap_or_default().to_owned();
        if self.ctx(&run_id).is_none() {
            // the run just cooked but its dir is unreadable: mechanically
            // dead — nothing can ever advance it (plan Task 5 ruling)
            self.dead_end_run(conn, now, &run_id, event.seq)?;
        }
        Ok(())
    }

    fn on_bead_created(
        &mut self,
        conn: &Connection,
        now: &str,
        event: &Event,
    ) -> Result<(), CoreError> {
        let Some(bead) = event.bead.as_deref() else {
            return Ok(());
        };
        self.maybe_claim_looping(conn, now, bead)
    }

    fn on_bead_closed(
        &mut self,
        conn: &Connection,
        now: &str,
        event: &Event,
    ) -> Result<(), CoreError> {
        let Some(bead) = event.bead.as_deref() else {
            return Ok(());
        };
        let outcome = event.data["outcome"].as_str().unwrap_or_default();
        // (1) a pass close may make looping anchors ready (spec §7.3
        // affected-subgraph recompute; plain dependents are converge's job)
        if outcome == "pass" {
            for ready in camp_core::readiness::newly_ready(conn, bead)? {
                self.maybe_claim_looping(conn, now, &ready)?;
            }
        }
        // (2) run-bead handling
        let Some(membership) = flow::run_membership(conn, bead)? else {
            return Ok(()); // plain bead
        };
        let Some(step_id) = membership.step_id else {
            // a run ROOT closed: advance any bond chain hanging off it
            return self.on_root_closed(conn, bead);
        };
        let Some(ctx) = self.ctx(&membership.run_id) else {
            return self.dead_end_run(conn, now, &membership.run_id, event.seq);
        };
        let Some(step_ref) = ctx.step_ref(&step_id) else {
            return Ok(()); // manifest/ledger drift: nothing mechanical to do
        };
        if step_ref.anchor == bead {
            // the STEP resolved
            if outcome == "pass" && step_ref.step.on_complete.is_some() {
                self.queue_fanout(PendingFanout {
                    run_id: ctx.run_id.clone(),
                    step_id: step_id.clone(),
                    anchor: bead.to_owned(),
                });
            }
            self.finalize_if_quiescent(conn, now, &ctx, event.seq)
        } else {
            // an ATTEMPT resolved: the mechanical loop advances
            self.on_attempt_closed(conn, now, &ctx, step_ref.step, bead, &event.data)
        }
    }

    /// A closed root that carries `bond:<anchor>:<i>` labels is a fan-out
    /// child: queue the parent anchor's fan-out so a sequential chain can
    /// cook its next item (the executor decides whether anything is due —
    /// a non-pass child halts the chain by cooking nothing).
    fn on_root_closed(&mut self, conn: &Connection, root: &str) -> Result<(), CoreError> {
        let Some(row) = camp_core::readiness::get_bead(conn, root)? else {
            return Ok(());
        };
        for label in &row.labels {
            let Some((parent_anchor, _index)) = flow::parse_bond_label(label) else {
                continue;
            };
            let Some(pm) = flow::run_membership(conn, parent_anchor)? else {
                continue;
            };
            let Some(parent_step) = pm.step_id else {
                continue;
            };
            self.queue_fanout(PendingFanout {
                run_id: pm.run_id,
                step_id: parent_step,
                anchor: parent_anchor.to_owned(),
            });
        }
        Ok(())
    }

    /// Claim a ready looping-step anchor for campd and create attempt 1.
    /// Guarded: only anchors, only looping steps, only open + ready — each
    /// append destroys its own precondition (bounded work, issue #17
    /// adjacency).
    fn maybe_claim_looping(
        &mut self,
        conn: &Connection,
        now: &str,
        bead: &str,
    ) -> Result<(), CoreError> {
        let Some(membership) = flow::run_membership(conn, bead)? else {
            return Ok(());
        };
        let Some(step_id) = membership.step_id else {
            return Ok(());
        };
        let Some(ctx) = self.ctx(&membership.run_id) else {
            return Ok(()); // dead-ended (or about to be) elsewhere
        };
        let Some(step_ref) = ctx.step_ref(&step_id) else {
            return Ok(());
        };
        if step_ref.anchor != bead || !flow::is_looping(step_ref.step) {
            return Ok(());
        }
        let Some(row) = camp_core::readiness::get_bead(conn, bead)? else {
            return Ok(());
        };
        if row.status != "open" || !camp_core::readiness::is_ready(conn, bead)? {
            return Ok(());
        }
        Ledger::append_on(
            conn,
            now,
            EventInput {
                kind: EventType::BeadClaimed,
                rig: Some(row.rig.clone()),
                actor: "campd".into(),
                bead: Some(bead.to_owned()),
                data: serde_json::json!({"session": "campd"}),
            },
        )?;
        let step = step_ref.step.clone();
        self.create_attempt(conn, now, &ctx, &step, &row, 1, None)?;
        Ok(())
    }

    /// Create attempt bead N for a looping step. The attempt carries the
    /// anchor's (var-substituted) title + description, the step's assignee
    /// routing hint, and — for respawns — the previous attempt's failure
    /// evidence (mechanical copying; the worker needs it).
    #[allow(clippy::too_many_arguments)]
    fn create_attempt(
        &mut self,
        conn: &Connection,
        now: &str,
        ctx: &RunContext,
        step: &Step,
        anchor_row: &BeadRow,
        attempt_no: usize,
        evidence: Option<String>,
    ) -> Result<(), CoreError> {
        let id = camp_core::id::next_bead_id(conn, prefix_of(&anchor_row.id)?)?;
        let base_description = flow::created_event_data(conn, &anchor_row.id)?
            .as_ref()
            .and_then(|d| d.get("description"))
            .and_then(|d| d.as_str())
            .unwrap_or_default()
            .to_owned();
        let input = attempt_bead_input(
            id,
            &anchor_row.rig,
            &ctx.run_id,
            step,
            &anchor_row.title,
            &base_description,
            attempt_no,
            evidence.as_deref(),
        );
        Ledger::append_on(conn, now, input)?;
        Ok(())
    }

    /// The worker's close of an attempt is the loop's input (spec §8.3):
    /// check steps queue the mechanical checker on pass and hard-fail
    /// otherwise (Decision 13); retry steps classify per gc's
    /// pass/hard/transient rules with the declared budget.
    fn on_attempt_closed(
        &mut self,
        conn: &Connection,
        now: &str,
        ctx: &RunContext,
        step: &Step,
        attempt_bead: &str,
        close_data: &serde_json::Value,
    ) -> Result<(), CoreError> {
        let anchor_id = ctx.anchors[&step.id].clone();
        let Some(anchor_row) = camp_core::readiness::get_bead(conn, &anchor_id)? else {
            return Ok(());
        };
        if anchor_row.status != "in_progress" || anchor_row.claimed_by.as_deref() != Some("campd") {
            // not campd's loop anymore (manual interference is visible in
            // the ledger); appending here would fight the operator
            return Ok(());
        }
        let outcome = close_data["outcome"].as_str().unwrap_or_default();
        let reason = close_data["reason"].as_str().unwrap_or("no reason given");
        let attempts = flow::attempts(conn, &ctx.run_id, &step.id, &anchor_id)?;
        let attempt_no = attempts
            .iter()
            .position(|b| b.id == attempt_bead)
            .map(|i| i + 1)
            .unwrap_or(attempts.len());

        if step.check.is_some() {
            if outcome == "pass" {
                self.queue_check(PendingCheck {
                    run_id: ctx.run_id.clone(),
                    step_id: step.id.clone(),
                    anchor: anchor_id,
                    attempt_bead: attempt_bead.to_owned(),
                    attempt_no: flow::check_runs_used(&attempts),
                });
                return Ok(());
            }
            // a worker failure on a check step is hard: the check budget
            // counts check runs, not worker failures (Decision 13)
            return close_anchor(
                conn,
                now,
                &anchor_row,
                "fail",
                Some("hard_fail"),
                None,
                &format!("attempt {attempt_no} failed: {reason}"),
            );
        }
        if let Some(retry) = &step.retry {
            if outcome == "pass" {
                return close_anchor(
                    conn,
                    now,
                    &anchor_row,
                    "pass",
                    None,
                    close_data.get("output").cloned(),
                    &format!("attempt {attempt_no} passed"),
                );
            }
            let transient =
                close_data.get("failure_class").and_then(|c| c.as_str()) == Some("transient");
            if transient {
                let used = flow::transient_fails_used(conn, &attempts)?;
                if used < retry.max_attempts {
                    let step = step.clone();
                    let evidence = format!("attempt {attempt_no} failed transient: {reason}");
                    self.create_attempt(
                        conn,
                        now,
                        ctx,
                        &step,
                        &anchor_row,
                        attempts.len() + 1,
                        Some(evidence),
                    )?;
                    return Ok(());
                }
                return close_anchor(
                    conn,
                    now,
                    &anchor_row,
                    "fail",
                    Some(retry.on_exhausted.as_str()),
                    None,
                    &format!("retry budget ({}) exhausted", retry.max_attempts),
                );
            }
            // hard fail: the worker said so
            return close_anchor(
                conn,
                now,
                &anchor_row,
                "fail",
                Some("hard_fail"),
                None,
                &format!("attempt {attempt_no} failed: {reason}"),
            );
        }
        Ok(()) // plain steps have no attempts; nothing mechanical to do
    }

    /// Finalization (plan Decision 3, approved): when the run is quiescent,
    /// close unreachable anchors `skipped`, close the root (outcome only),
    /// and append `run.finalized` with the run-level disposition and its
    /// cause — all in this same cursor transaction.
    fn finalize_if_quiescent(
        &mut self,
        conn: &Connection,
        now: &str,
        ctx: &RunContext,
        cause_seq: i64,
    ) -> Result<(), CoreError> {
        match flow::finalization(conn, ctx)? {
            RunVerdict::NotQuiescent => Ok(()),
            RunVerdict::Finalize {
                outcome,
                disposition,
                soft_failed,
                skipped,
                to_skip,
            } => {
                for bead in &to_skip {
                    Ledger::append_on(
                        conn,
                        now,
                        EventInput {
                            kind: EventType::BeadClosed,
                            rig: Some(ctx.rig.clone()),
                            actor: "campd".into(),
                            bead: Some(bead.clone()),
                            data: serde_json::json!({
                                "outcome": "skipped",
                                "reason": "needs cannot be satisfied",
                            }),
                        },
                    )?;
                }
                Ledger::append_on(
                    conn,
                    now,
                    EventInput {
                        kind: EventType::BeadClosed,
                        rig: Some(ctx.rig.clone()),
                        actor: "campd".into(),
                        bead: Some(ctx.root.clone()),
                        data: serde_json::json!({
                            "outcome": outcome,
                            "reason": "run finalized",
                        }),
                    },
                )?;
                Ledger::append_on(
                    conn,
                    now,
                    EventInput {
                        kind: EventType::RunFinalized,
                        rig: Some(ctx.rig.clone()),
                        actor: "campd".into(),
                        bead: Some(ctx.root.clone()),
                        data: serde_json::json!({
                            "run_id": ctx.run_id,
                            "root": ctx.root,
                            "outcome": outcome,
                            "final_disposition": disposition,
                            "cause_seq": cause_seq,
                            "soft_failed": soft_failed,
                            "skipped": skipped,
                        }),
                    },
                )?;
                Ok(())
            }
        }
    }

    /// A run whose pinned dir is unreadable can never advance: close every
    /// open run bead `skipped`, the root `fail`, and finalize — the honest
    /// mechanical dead-end, evented, never silent (plan Task 5 ruling).
    /// Cursor-atomic here; reconcile uses the same builder as one batch.
    fn dead_end_run(
        &mut self,
        conn: &Connection,
        now: &str,
        run_id: &str,
        cause_seq: i64,
    ) -> Result<(), CoreError> {
        for input in flow::dead_end_inputs(
            conn,
            run_id,
            cause_seq,
            "run dir unreadable; the run cannot advance",
        )? {
            Ledger::append_on(conn, now, input)?;
        }
        Ok(())
    }

    fn queue_check(&mut self, pending: PendingCheck) {
        if !self.pending_checks.contains(&pending) {
            self.pending_checks.push(pending);
        }
    }

    fn queue_fanout(&mut self, pending: PendingFanout) {
        if !self.pending_fanouts.contains(&pending) {
            self.pending_fanouts.push(pending);
        }
    }

    /// Test observability (production drains via the Task 6/7 executors).
    #[cfg(test)]
    pub fn pending_check_queue(&self) -> &[PendingCheck] {
        &self.pending_checks
    }

    #[cfg(test)]
    #[allow(dead_code)] // consumed by the Task 7 fan-out tests
    pub fn pending_fanout_queue(&self) -> &[PendingFanout] {
        &self.pending_fanouts
    }
}

/// The bead id prefix of an existing bead id (attempt beads share their
/// anchor's rig prefix).
fn prefix_of(bead_id: &str) -> Result<&str, CoreError> {
    camp_core::id::parse_bead_id(bead_id)
        .map(|(prefix, _)| prefix)
        .ok_or_else(|| CoreError::Corrupt(format!("{bead_id:?} is not a bead id")))
}

/// The bead.created input for a looping-step attempt (shared by the
/// cursor-atomic processor path and the check-reap batch path).
#[allow(clippy::too_many_arguments)]
fn attempt_bead_input(
    id: String,
    rig: &str,
    run_id: &str,
    step: &Step,
    anchor_title: &str,
    base_description: &str,
    attempt_no: usize,
    evidence: Option<&str>,
) -> EventInput {
    let description = match evidence {
        Some(evidence) if base_description.is_empty() => evidence.to_owned(),
        Some(evidence) => format!("{base_description}\n\n{evidence}"),
        None => base_description.to_owned(),
    };
    let mut data = serde_json::json!({
        "title": format!("{anchor_title} (attempt {attempt_no})"),
        "run_id": run_id,
        "step_id": step.id,
    });
    if !description.is_empty() {
        data["description"] = serde_json::json!(description);
    }
    if let Some(assignee) = &step.assignee {
        data["assignee"] = serde_json::json!(assignee);
    }
    EventInput {
        kind: EventType::BeadCreated,
        rig: Some(rig.to_owned()),
        actor: "campd".into(),
        bead: Some(id),
        data,
    }
}

/// The bond children already cooked for an anchor, by index — one
/// beads-table label query (review LOW 4; the events-scan predecessor
/// cost O(total runs) per chain link).
fn existing_bond_children(
    ledger: &Ledger,
    anchor: &str,
) -> Result<std::collections::BTreeMap<usize, BeadRow>, CoreError> {
    ledger.bond_children(anchor)
}

/// A fan-out-level failure: evented on the anchor (invariant 5 — campd
/// has no caller), fan-out dropped. dispatch.failed is the honest name:
/// campd could not dispatch the declared follow-on work.
fn fanout_failure(
    ledger: &mut Ledger,
    fanout: &PendingFanout,
    ctx: &RunContext,
    reason: &str,
) -> Result<()> {
    ledger.append(EventInput {
        kind: EventType::DispatchFailed,
        rig: Some(ctx.rig.clone()),
        actor: "campd".into(),
        bead: Some(fanout.anchor.clone()),
        data: serde_json::json!({
            "reason": format!("on_complete fan-out: {reason}"),
        }),
    })?;
    Ok(())
}

/// The last ~2 KB of a check log — the mechanical evidence copied into the
/// next iteration's description (nothing hidden; the worker needs it).
fn log_tail(path: &Path) -> String {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let tail_start = text.len().saturating_sub(2048);
            // don't split a UTF-8 char
            let mut start = tail_start;
            while start < text.len() && !text.is_char_boundary(start) {
                start += 1;
            }
            text[start..].to_owned()
        }
        Err(_) => String::new(), // the log path is already in the event
    }
}

/// Close a looping-step anchor with campd's verdict. The disposition is
/// close-vocabulary (`hard_fail|soft_fail`) only; the run-level "pass"
/// lives in run.finalized (plan Decision 3 / review Blocker A).
fn close_anchor(
    conn: &Connection,
    now: &str,
    anchor: &BeadRow,
    outcome: &str,
    final_disposition: Option<&str>,
    output: Option<serde_json::Value>,
    reason: &str,
) -> Result<(), CoreError> {
    let mut data = serde_json::json!({ "outcome": outcome, "reason": reason });
    if let Some(disposition) = final_disposition {
        data["final_disposition"] = serde_json::json!(disposition);
    }
    if let Some(output) = output {
        data["output"] = output;
    }
    Ledger::append_on(
        conn,
        now,
        EventInput {
            kind: EventType::BeadClosed,
            rig: Some(anchor.rig.clone()),
            actor: "campd".into(),
            bead: Some(anchor.id.clone()),
            data,
        },
    )?;
    Ok(())
}

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
            (
                ExitStatus::from_raw(0),
                EventType::SessionStopped,
                Some(0),
                None,
            ),
            (
                ExitStatus::from_raw(7 << 8),
                EventType::SessionCrashed,
                Some(7),
                None,
            ),
            // SIGKILL: shells report 137, the wait status is signal 9 (F4)
            (
                ExitStatus::from_raw(9),
                EventType::SessionCrashed,
                None,
                Some(9),
            ),
            (
                ExitStatus::from_raw(15),
                EventType::SessionCrashed,
                None,
                Some(15),
            ),
        ];
        for (status, kind, code, signal) in cases {
            assert_eq!(classify(status), (kind, code, signal), "status {status:?}");
        }
    }

    /// Review finding 2 (PR #51): a nudge write into a full pipe of a
    /// worker that is not reading must fail BOUNDED, never wedge campd's
    /// single-threaded event loop (Request::Nudge made this path
    /// operator-triggerable over the socket). After the bounded failure
    /// the pipe may hold a torn partial line, so it must be dropped — a
    /// later nudge sees NoPipe (resume path), never interleaved garbage.
    #[test]
    fn a_full_stdin_pipe_fails_the_nudge_bounded_instead_of_wedging_campd() {
        // NOTE: do NOT hold spawn_probe_guard here — test_insert_held_sleeper
        // acquires it per call, and the guard mutex is non-reentrant, so a
        // test-level hold would self-deadlock (same rule as
        // a_cap_full_patrol_respawn_queues_and_retries_when_a_slot_frees).
        let dir = tempfile::tempdir().unwrap();
        let config = CampConfig::parse("[camp]\nname = \"t\"\n").unwrap();
        let mut dispatcher = Dispatcher::new(
            CampDir {
                root: dir.path().to_path_buf(),
            },
            config,
        );
        // a held child that NEVER reads its stdin
        let pid = dispatcher.test_insert_held_sleeper(dir.path(), "t/dev/1", "gc-1");

        // far larger than any OS pipe buffer: the write cannot complete
        let big = "x".repeat(2 * 1024 * 1024);
        let started = std::time::Instant::now();
        let outcome = dispatcher.nudge_via_stdin("t/dev/1", &big);
        assert!(
            matches!(outcome, NudgeOutcome::Failed(_)),
            "a full pipe must fail the nudge: {outcome:?}"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "the nudge write must be bounded, not a wedge ({}s)",
            started.elapsed().as_secs()
        );
        // the torn pipe is dead: no later turn may interleave into it
        assert!(
            matches!(
                dispatcher.nudge_via_stdin("t/dev/1", "again"),
                NudgeOutcome::NoPipe
            ),
            "after a failed write the pipe must be dropped (NoPipe)"
        );
        dispatcher.test_kill_and_wait(pid);
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
            work_outcome: None,
            dispatch_failure: None,
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

    use crate::campdir::CampDir;
    use camp_core::event::EventInput;
    use camp_core::ledger::Ledger;

    fn temp_ledger() -> (tempfile::TempDir, Ledger) {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::open(&dir.path().join("camp.db")).unwrap();
        (dir, ledger)
    }

    fn wake_session(l: &mut Ledger, name: &str) {
        l.append(EventInput {
            kind: EventType::SessionWoke,
            rig: Some("gc".into()),
            actor: "campd".into(),
            bead: None,
            data: serde_json::json!({"name": name, "agent": "dev"}),
        })
        .unwrap();
    }

    fn exited_child() -> std::process::Child {
        // serialized against socket-probe tests (see spawn_probe_guard)
        let _spawning = crate::daemon::spawn_probe_guard();
        let mut child = std::process::Command::new("true").spawn().unwrap();
        child.wait().unwrap(); // try_wait re-returns the cached status
        child
    }

    fn count(l: &Ledger, kind: &str) -> usize {
        l.events_range(1, None)
            .unwrap()
            .iter()
            .filter(|e| e.kind.as_str() == kind)
            .count()
    }

    /// PR #14 review finding 1: a failed worktree-disposition append must
    /// not orphan the disposition — the worker stays tracked, the retry
    /// skips the already-committed session end (no fold double-end wedge),
    /// and the disposition event lands on the next wake.
    #[test]
    fn a_failed_disposition_append_retries_without_double_ending_the_session() {
        let (dir, mut ledger) = temp_ledger();
        wake_session(&mut ledger, "t/dev/1");
        let worktree = dir.path().join("wt-gc-9");
        std::fs::create_dir_all(&worktree).unwrap();

        let mut dispatcher = Dispatcher::new(
            CampDir {
                root: dir.path().to_path_buf(),
            },
            CampConfig::parse("[camp]\nname = \"t\"\n").unwrap(),
        );
        let child = exited_child();
        let pid = child.id();
        dispatcher.children.insert(
            pid,
            Worker {
                child,
                session: "t/dev/1".into(),
                bead: "gc-9".into(), // does NOT exist: the kept-append fails
                rig: "gc".into(),
                rig_path: dir.path().to_path_buf(),
                worktree: Some(worktree.clone()),
                end_recorded: false,
                stdin: None,
                released: None,
                patrol_kill: None,
                kill_reason: None,
            },
        );

        // First reap: the session end commits, the disposition append
        // fails (unknown bead) — the worker must STAY tracked.
        let err = dispatcher.reap(&mut ledger);
        assert!(err.is_err(), "disposition failure must surface");
        assert_eq!(count(&ledger, "session.stopped"), 1);
        assert_eq!(count(&ledger, "worktree.kept"), 0);
        assert!(
            dispatcher.children.contains_key(&pid),
            "the worker must not be forgotten before its disposition lands"
        );

        // The failure cause resolves (the bead appears); the retry must
        // land the disposition WITHOUT re-appending the session end.
        ledger
            .append(EventInput {
                kind: EventType::BeadCreated,
                rig: Some("gc".into()),
                actor: "test".into(),
                bead: Some("gc-9".into()),
                data: serde_json::json!({"title": "t"}),
            })
            .unwrap();
        dispatcher.reap(&mut ledger).unwrap();
        assert_eq!(count(&ledger, "worktree.kept"), 1);
        assert_eq!(
            count(&ledger, "session.stopped"),
            1,
            "the retry must not double-end the session"
        );
        assert!(dispatcher.children.is_empty(), "pid must not wedge");
    }

    /// PR #14 review finding 7 (unit half): a clean close whose worktree
    /// removal fails keeps the worktree with the git error as the reason.
    #[test]
    fn a_failed_removal_on_clean_close_keeps_the_worktree_with_the_reason() {
        let (dir, mut ledger) = temp_ledger();
        wake_session(&mut ledger, "t/dev/2");
        // closed-pass bead
        ledger
            .append(EventInput {
                kind: EventType::BeadCreated,
                rig: Some("gc".into()),
                actor: "test".into(),
                bead: Some("gc-1".into()),
                data: serde_json::json!({"title": "t"}),
            })
            .unwrap();
        ledger
            .append(EventInput {
                kind: EventType::BeadClosed,
                rig: Some("gc".into()),
                actor: "test".into(),
                bead: Some("gc-1".into()),
                data: serde_json::json!({"outcome": "pass"}),
            })
            .unwrap();
        // the worktree dir exists, but rig_path is not a git repo, so
        // `git worktree remove` must fail
        let worktree = dir.path().join("wt-gc-1");
        std::fs::create_dir_all(&worktree).unwrap();

        let mut dispatcher = Dispatcher::new(
            CampDir {
                root: dir.path().to_path_buf(),
            },
            CampConfig::parse("[camp]\nname = \"t\"\n").unwrap(),
        );
        let child = exited_child();
        dispatcher.children.insert(
            child.id(),
            Worker {
                child,
                session: "t/dev/2".into(),
                bead: "gc-1".into(),
                rig: "gc".into(),
                rig_path: dir.path().to_path_buf(), // not a git repo
                worktree: Some(worktree.clone()),
                end_recorded: false,
                stdin: None,
                released: None,
                patrol_kill: None,
                kill_reason: None,
            },
        );
        {
            let _spawning = crate::daemon::spawn_probe_guard();
            dispatcher.reap(&mut ledger).unwrap();
        }
        assert!(worktree.exists(), "a failed removal keeps the worktree");
        let events = ledger.events_range(1, None).unwrap();
        let kept = events
            .iter()
            .find(|e| e.kind.as_str() == "worktree.kept")
            .expect("worktree.kept must record the failed removal");
        assert!(
            kept.data["reason"]
                .as_str()
                .unwrap()
                .contains("removal failed"),
            "reason was: {}",
            kept.data["reason"]
        );
        assert_eq!(count(&ledger, "bead.worktree.reaped"), 0);
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
            assert!(
                err.contains(needle),
                "route error must name {needle}: {err}"
            );
        }
    }

    // ---- Phase 9 Task 5: GraphRuntime processor hooks --------------------

    use camp_core::clock::FixedClock;
    use camp_core::formula::CookedRun;
    use camp_core::formula::runtime::runs_dir;

    /// A camp root with one rig; ledger + orders runtime + graph runtime.
    fn graph_fixture() -> (
        tempfile::TempDir,
        Ledger,
        super::super::orders::OrdersRuntime,
        GraphRuntime,
    ) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("repo")).unwrap();
        std::fs::write(
            dir.path().join("camp.toml"),
            format!(
                "[camp]\nname = \"t\"\n\n[[rigs]]\nname = \"gc\"\npath = \"{}\"\nprefix = \"gc\"\n",
                dir.path().join("repo").display()
            ),
        )
        .unwrap();
        let ledger = Ledger::open_with_clock(
            &dir.path().join("camp.db"),
            Box::new(FixedClock::new("2026-07-07T12:00:00Z")),
        )
        .unwrap();
        let rt = super::super::orders::OrdersRuntime::build(
            dir.path(),
            "2026-07-07T12:00:00Z".parse().unwrap(),
            jiff::tz::TimeZone::UTC,
        )
        .unwrap();
        let config =
            CampConfig::parse(&std::fs::read_to_string(dir.path().join("camp.toml")).unwrap())
                .unwrap();
        let graph = GraphRuntime::new(dir.path().to_path_buf(), &config);
        (dir, ledger, rt, graph)
    }

    fn cook_into(
        dir: &tempfile::TempDir,
        ledger: &mut Ledger,
        name: &str,
        toml: &str,
    ) -> CookedRun {
        let path = dir.path().join(format!("{name}.toml"));
        std::fs::write(&path, toml).unwrap();
        let formula = camp_core::formula::parse_and_validate(&path).unwrap();
        let rig = camp_core::config::RigConfig {
            name: "gc".into(),
            path: dir.path().join("repo"),
            prefix: "gc".into(),
            default_agent: None,
        };
        camp_core::formula::cook(ledger, &formula, &runs_dir(dir.path()), &rig, "test").unwrap()
    }

    fn settle_graph(
        ledger: &mut Ledger,
        rt: &mut super::super::orders::OrdersRuntime,
        graph: &mut GraphRuntime,
    ) {
        let mut readiness = crate::daemon::cursor::ReadinessProcessor::default();
        let clock = FixedClock::new("2026-07-07T12:00:01Z");
        // Phase 11: settle threads a patrol runtime too (unwatched/empty here).
        let cfg = CampConfig::parse("[camp]\nname = \"t\"\n").unwrap();
        let patrol_config = camp_core::patrol::PatrolConfig::from_section(&cfg.patrol).unwrap();
        let mut patrol = crate::daemon::patrol::PatrolRuntime::new(patrol_config, &cfg);
        // cp-0: settle threads a read-channel runtime too (empty here). Its
        // sessions dir is a throwaway temp dir — no sessions are registered
        // in the graph tests, and OrdersRuntime.camp_root is private.
        let read_dir = tempfile::tempdir().unwrap();
        let mut read_channel = crate::daemon::read_channel::ReadChannelRuntime::new(
            read_dir.path().to_path_buf(),
            256 * 1024 * 1024,
        )
        .unwrap();
        super::super::orders::settle(
            ledger,
            &mut readiness,
            rt,
            &clock,
            graph,
            &mut patrol,
            &mut read_channel,
        )
        .unwrap();
    }

    fn append_close(l: &mut Ledger, bead: &str, data: serde_json::Value) -> i64 {
        l.append(EventInput {
            kind: EventType::BeadClosed,
            rig: Some("gc".into()),
            actor: "session:fake".into(),
            bead: Some(bead.into()),
            data,
        })
        .unwrap()
    }

    const RETRY_SOFT: &str = "formula = \"retry-soft\"\n\n[requires]\nformula_compiler = \">=2.0.0\"\n\n[[steps]]\nid = \"fetch\"\ntitle = \"Fetch\"\n\n[steps.retry]\nmax_attempts = 2\non_exhausted = \"soft_fail\"\n";
    const RETRY_HARD: &str = "formula = \"retry-hard\"\n\n[requires]\nformula_compiler = \">=2.0.0\"\n\n[[steps]]\nid = \"fetch\"\ntitle = \"Fetch\"\n\n[steps.retry]\nmax_attempts = 2\non_exhausted = \"hard_fail\"\n";
    const CHECKED: &str = "formula = \"checked\"\n\n[requires]\nformula_compiler = \">=2.0.0\"\n\n[[steps]]\nid = \"impl\"\ntitle = \"Implement\"\n\n[steps.check]\nmax_attempts = 3\n\n[steps.check.check]\nmode = \"exec\"\npath = \"verify.sh\"\ntimeout = \"1m\"\n";
    const TWO_STEP: &str = "formula = \"two-step\"\n\n[[steps]]\nid = \"a\"\ntitle = \"A\"\n\n[[steps]]\nid = \"b\"\ntitle = \"B\"\nneeds = [\"a\"]\n";

    fn events_named(l: &Ledger, kind: &str) -> Vec<camp_core::event::Event> {
        l.events_range(1, None)
            .unwrap()
            .into_iter()
            .filter(|e| e.kind.as_str() == kind)
            .collect()
    }

    #[test]
    fn a_ready_looping_anchor_is_claimed_with_attempt_one() {
        let (dir, mut ledger, mut rt, mut graph) = graph_fixture();
        let cooked = cook_into(&dir, &mut ledger, "retry-soft", RETRY_SOFT);
        settle_graph(&mut ledger, &mut rt, &mut graph);
        let anchor = ledger
            .get_bead(&cooked.step_beads["fetch"])
            .unwrap()
            .unwrap();
        assert_eq!(anchor.status, "in_progress");
        assert_eq!(anchor.claimed_by.as_deref(), Some("campd"));
        let attempts = ledger
            .step_attempts(&cooked.run_id, "fetch", &anchor.id)
            .unwrap();
        assert_eq!(attempts.len(), 1, "exactly one attempt bead");
        assert!(attempts[0].title.contains("(attempt 1)"));
        // the ATTEMPT is dispatchable; the claimed anchor is not
        let dispatchable: Vec<String> = ledger
            .dispatchable_beads()
            .unwrap()
            .into_iter()
            .map(|b| b.id)
            .collect();
        assert!(dispatchable.contains(&attempts[0].id), "{dispatchable:?}");
        assert!(!dispatchable.contains(&anchor.id), "{dispatchable:?}");
    }

    #[test]
    fn retry_classification_pass_hard_transient() {
        // pass: anchor closes pass with the attempt output copied; the
        // single-step run finalizes
        let (dir, mut ledger, mut rt, mut graph) = graph_fixture();
        let cooked = cook_into(&dir, &mut ledger, "retry-soft", RETRY_SOFT);
        settle_graph(&mut ledger, &mut rt, &mut graph);
        let anchor_id = cooked.step_beads["fetch"].clone();
        let attempt = ledger
            .step_attempts(&cooked.run_id, "fetch", &anchor_id)
            .unwrap()[0]
            .id
            .clone();
        append_close(
            &mut ledger,
            &attempt,
            serde_json::json!({"outcome":"pass","output":{"items":[1]}}),
        );
        settle_graph(&mut ledger, &mut rt, &mut graph);
        let anchor = ledger.get_bead(&anchor_id).unwrap().unwrap();
        assert_eq!(anchor.status, "closed");
        assert_eq!(anchor.outcome.as_deref(), Some("pass"));
        let close = ledger.close_event_data(&anchor_id).unwrap().unwrap();
        assert_eq!(close["output"]["items"][0], 1, "output copied to the step");
        let finalized = events_named(&ledger, "run.finalized");
        assert_eq!(finalized.len(), 1);
        assert_eq!(finalized[0].data["outcome"], "pass");
        assert_eq!(finalized[0].data["final_disposition"], "pass");
        let root = ledger.get_bead(&cooked.root_bead).unwrap().unwrap();
        assert_eq!(root.outcome.as_deref(), Some("pass"));
        assert!(
            ledger.close_event_data(&cooked.root_bead).unwrap().unwrap()["final_disposition"]
                .is_null(),
            "root closes carry outcome only (Decision 3)"
        );

        // hard fail: the worker said fail without a classification
        let (dir, mut ledger, mut rt, mut graph) = graph_fixture();
        let cooked = cook_into(&dir, &mut ledger, "retry-soft", RETRY_SOFT);
        settle_graph(&mut ledger, &mut rt, &mut graph);
        let anchor_id = cooked.step_beads["fetch"].clone();
        let attempt = ledger
            .step_attempts(&cooked.run_id, "fetch", &anchor_id)
            .unwrap()[0]
            .id
            .clone();
        append_close(
            &mut ledger,
            &attempt,
            serde_json::json!({"outcome":"fail","reason":"broke"}),
        );
        settle_graph(&mut ledger, &mut rt, &mut graph);
        let close = ledger.close_event_data(&anchor_id).unwrap().unwrap();
        assert_eq!(close["outcome"], "fail");
        assert_eq!(close["final_disposition"], "hard_fail");
        assert_eq!(
            events_named(&ledger, "run.finalized")[0].data["final_disposition"],
            "hard_fail"
        );

        // transient with budget left: attempt 2 appears, anchor stays campd's
        let (dir, mut ledger, mut rt, mut graph) = graph_fixture();
        let cooked = cook_into(&dir, &mut ledger, "retry-soft", RETRY_SOFT);
        settle_graph(&mut ledger, &mut rt, &mut graph);
        let anchor_id = cooked.step_beads["fetch"].clone();
        let attempt = ledger
            .step_attempts(&cooked.run_id, "fetch", &anchor_id)
            .unwrap()[0]
            .id
            .clone();
        append_close(
            &mut ledger,
            &attempt,
            serde_json::json!({"outcome":"fail","failure_class":"transient","reason":"net"}),
        );
        settle_graph(&mut ledger, &mut rt, &mut graph);
        let attempts = ledger
            .step_attempts(&cooked.run_id, "fetch", &anchor_id)
            .unwrap();
        assert_eq!(attempts.len(), 2, "the respawn attempt exists");
        assert!(attempts[1].title.contains("(attempt 2)"));
        let created = camp_core::formula::runtime::created_event_data(
            // description carries the failure evidence, mechanically copied
            &rusqlite::Connection::open(dir.path().join("camp.db")).unwrap(),
            &attempts[1].id,
        )
        .unwrap()
        .unwrap();
        assert!(
            created["description"].as_str().unwrap().contains("net"),
            "{created}"
        );
        assert_eq!(
            ledger.get_bead(&anchor_id).unwrap().unwrap().status,
            "in_progress"
        );
    }

    #[test]
    fn retry_exhaustion_honors_on_exhausted() {
        for (name, toml, disposition, run_outcome, run_disposition) in [
            ("retry-soft", RETRY_SOFT, "soft_fail", "pass", "soft_fail"),
            ("retry-hard", RETRY_HARD, "hard_fail", "fail", "hard_fail"),
        ] {
            let (dir, mut ledger, mut rt, mut graph) = graph_fixture();
            let cooked = cook_into(&dir, &mut ledger, name, toml);
            settle_graph(&mut ledger, &mut rt, &mut graph);
            let anchor_id = cooked.step_beads["fetch"].clone();
            for _ in 0..2 {
                let attempts = ledger
                    .step_attempts(&cooked.run_id, "fetch", &anchor_id)
                    .unwrap();
                let open = attempts
                    .iter()
                    .find(|b| b.status == "open")
                    .unwrap()
                    .id
                    .clone();
                append_close(
                    &mut ledger,
                    &open,
                    serde_json::json!({"outcome":"fail","failure_class":"transient"}),
                );
                settle_graph(&mut ledger, &mut rt, &mut graph);
            }
            let close = ledger.close_event_data(&anchor_id).unwrap().unwrap();
            assert_eq!(close["outcome"], "fail", "{name}");
            assert_eq!(close["final_disposition"], disposition.to_owned(), "{name}");
            assert!(
                close["reason"]
                    .as_str()
                    .unwrap()
                    .contains("retry budget (2) exhausted"),
                "{name}: {close}"
            );
            let finalized = events_named(&ledger, "run.finalized");
            assert_eq!(finalized.len(), 1, "{name}");
            assert_eq!(
                finalized[0].data["outcome"],
                run_outcome.to_owned(),
                "{name}"
            );
            assert_eq!(
                finalized[0].data["final_disposition"],
                run_disposition.to_owned(),
                "{name}"
            );
            // exactly 2 attempts ever: the budget bounds the loop
            assert_eq!(
                ledger
                    .step_attempts(&cooked.run_id, "fetch", &anchor_id)
                    .unwrap()
                    .len(),
                2,
                "{name}"
            );
        }
    }

    #[test]
    fn finalization_closes_root_with_cause_and_skips_unreachable() {
        let (dir, mut ledger, mut rt, mut graph) = graph_fixture();
        let cooked = cook_into(&dir, &mut ledger, "two-step", TWO_STEP);
        settle_graph(&mut ledger, &mut rt, &mut graph);
        // a worker hard-fails plain step a; b can never run
        let cause_seq = append_close(
            &mut ledger,
            &cooked.step_beads["a"],
            serde_json::json!({"outcome":"fail","reason":"broke"}),
        );
        settle_graph(&mut ledger, &mut rt, &mut graph);
        let b = ledger.get_bead(&cooked.step_beads["b"]).unwrap().unwrap();
        assert_eq!(b.outcome.as_deref(), Some("skipped"));
        let root = ledger.get_bead(&cooked.root_bead).unwrap().unwrap();
        assert_eq!(root.outcome.as_deref(), Some("fail"));
        let finalized = events_named(&ledger, "run.finalized");
        assert_eq!(finalized.len(), 1);
        assert_eq!(finalized[0].data["cause_seq"], cause_seq);
        assert_eq!(finalized[0].data["final_disposition"], "hard_fail");
        assert_eq!(finalized[0].data["skipped"][0], "b");
    }

    #[test]
    fn a_passing_check_step_attempt_queues_a_check_not_an_anchor_close() {
        let (dir, mut ledger, mut rt, mut graph) = graph_fixture();
        let cooked = cook_into(&dir, &mut ledger, "checked", CHECKED);
        settle_graph(&mut ledger, &mut rt, &mut graph);
        let anchor_id = cooked.step_beads["impl"].clone();
        let attempt = ledger
            .step_attempts(&cooked.run_id, "impl", &anchor_id)
            .unwrap()[0]
            .id
            .clone();
        append_close(&mut ledger, &attempt, serde_json::json!({"outcome":"pass"}));
        settle_graph(&mut ledger, &mut rt, &mut graph);
        assert_eq!(
            ledger.get_bead(&anchor_id).unwrap().unwrap().status,
            "in_progress",
            "the check verdict, not the worker, resolves a check step"
        );
        let queue = graph.pending_check_queue();
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].attempt_no, 1);
        assert_eq!(queue[0].anchor, anchor_id);
        assert_eq!(queue[0].attempt_bead, attempt);
    }

    // ---- Phase 9 Task 6: check execution --------------------------------

    /// Test-only: block until every live check child exits, then reap.
    fn wait_and_reap_checks(graph: &mut GraphRuntime, ledger: &mut Ledger) {
        for check in graph.check_children.values_mut() {
            let _ = check.child.wait();
        }
        graph.reap_checks(ledger).unwrap();
    }

    /// Cook a checked run, close attempt 1 pass, and settle so the check
    /// is queued. Returns (anchor, attempt) bead ids.
    fn checked_run_with_queued_check(
        dir: &tempfile::TempDir,
        ledger: &mut Ledger,
        rt: &mut super::super::orders::OrdersRuntime,
        graph: &mut GraphRuntime,
        script: &str,
        toml: &str,
    ) -> (String, String, CookedRun) {
        let script_path = dir.path().join("repo/verify.sh");
        std::fs::write(&script_path, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let cooked = cook_into(dir, ledger, "checked", toml);
        settle_graph(ledger, rt, graph);
        let anchor_id = cooked.step_beads["impl"].clone();
        let attempt = ledger
            .step_attempts(&cooked.run_id, "impl", &anchor_id)
            .unwrap()[0]
            .id
            .clone();
        append_close(
            ledger,
            &attempt,
            serde_json::json!({"outcome":"pass","output":{"n":1}}),
        );
        settle_graph(ledger, rt, graph);
        (anchor_id, attempt, cooked)
    }

    #[test]
    fn a_passing_check_closes_the_anchor_with_output() {
        let _spawning = crate::daemon::spawn_probe_guard();
        let (dir, mut ledger, mut rt, mut graph) = graph_fixture();
        let (anchor_id, attempt, cooked) = checked_run_with_queued_check(
            &dir,
            &mut ledger,
            &mut rt,
            &mut graph,
            "#!/bin/sh\necho verifying\nexit 0\n",
            CHECKED,
        );
        graph.execute(&mut ledger).unwrap();
        wait_and_reap_checks(&mut graph, &mut ledger);
        let passed = events_named(&ledger, "check.passed");
        assert_eq!(passed.len(), 1);
        assert_eq!(passed[0].data["attempt"], 1);
        assert_eq!(passed[0].bead.as_deref(), Some(attempt.as_str()));
        let close = ledger.close_event_data(&anchor_id).unwrap().unwrap();
        assert_eq!(close["outcome"], "pass");
        assert_eq!(close["output"]["n"], 1, "attempt output copied to the step");
        // the settle that follows the reap finalizes the run
        settle_graph(&mut ledger, &mut rt, &mut graph);
        assert_eq!(events_named(&ledger, "run.finalized").len(), 1);
        assert_eq!(
            ledger
                .get_bead(&cooked.root_bead)
                .unwrap()
                .unwrap()
                .outcome
                .as_deref(),
            Some("pass")
        );
        assert!(ledger.refold_check().unwrap().drift.is_empty());
    }

    #[test]
    fn a_failing_check_with_budget_creates_the_next_attempt_with_evidence() {
        let _spawning = crate::daemon::spawn_probe_guard();
        let (dir, mut ledger, mut rt, mut graph) = graph_fixture();
        let (anchor_id, _attempt, cooked) = checked_run_with_queued_check(
            &dir,
            &mut ledger,
            &mut rt,
            &mut graph,
            "#!/bin/sh\necho boom goes the check\nexit 1\n",
            CHECKED,
        );
        graph.execute(&mut ledger).unwrap();
        wait_and_reap_checks(&mut graph, &mut ledger);
        let failed = events_named(&ledger, "check.failed");
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].data["attempt"], 1);
        assert_eq!(failed[0].data["exit_code"], 1);
        assert!(
            failed[0].data["log"]
                .as_str()
                .unwrap()
                .contains("impl-attempt-1.log")
        );
        let attempts = ledger
            .step_attempts(&cooked.run_id, "impl", &anchor_id)
            .unwrap();
        assert_eq!(attempts.len(), 2, "the next iteration bead exists");
        let created = ledger.created_event_data(&attempts[1].id).unwrap().unwrap();
        let description = created["description"].as_str().unwrap();
        assert!(
            description.contains("check failed (attempt 1)"),
            "{description}"
        );
        assert!(
            description.contains("boom goes the check"),
            "log tail copied: {description}"
        );
        assert_eq!(
            ledger.get_bead(&anchor_id).unwrap().unwrap().status,
            "in_progress",
            "the loop continues"
        );
    }

    #[test]
    fn check_budget_exhaustion_fails_the_anchor() {
        const CHECKED_ONE: &str = "formula = \"checked\"\n\n[requires]\nformula_compiler = \">=2.0.0\"\n\n[[steps]]\nid = \"impl\"\ntitle = \"Implement\"\n\n[steps.check]\nmax_attempts = 1\n\n[steps.check.check]\nmode = \"exec\"\npath = \"verify.sh\"\ntimeout = \"1m\"\n";
        let _spawning = crate::daemon::spawn_probe_guard();
        let (dir, mut ledger, mut rt, mut graph) = graph_fixture();
        let (anchor_id, _attempt, _cooked) = checked_run_with_queued_check(
            &dir,
            &mut ledger,
            &mut rt,
            &mut graph,
            "#!/bin/sh\nexit 1\n",
            CHECKED_ONE,
        );
        graph.execute(&mut ledger).unwrap();
        wait_and_reap_checks(&mut graph, &mut ledger);
        let close = ledger.close_event_data(&anchor_id).unwrap().unwrap();
        assert_eq!(close["outcome"], "fail");
        assert_eq!(close["final_disposition"], "hard_fail");
        assert!(
            close["reason"]
                .as_str()
                .unwrap()
                .contains("check budget (1) exhausted"),
            "{close}"
        );
        settle_graph(&mut ledger, &mut rt, &mut graph);
        let finalized = events_named(&ledger, "run.finalized");
        assert_eq!(finalized.len(), 1, "check exhaustion fails the run");
        assert_eq!(finalized[0].data["outcome"], "fail");
        assert!(ledger.refold_check().unwrap().drift.is_empty());
    }

    #[test]
    fn an_expired_check_is_killed_and_counts_as_a_failed_iteration() {
        const CHECKED_FAST: &str = "formula = \"checked\"\n\n[requires]\nformula_compiler = \">=2.0.0\"\n\n[[steps]]\nid = \"impl\"\ntitle = \"Implement\"\n\n[steps.check]\nmax_attempts = 3\n\n[steps.check.check]\nmode = \"exec\"\npath = \"verify.sh\"\ntimeout = \"200ms\"\n";
        let _spawning = crate::daemon::spawn_probe_guard();
        let (dir, mut ledger, mut rt, mut graph) = graph_fixture();
        let (anchor_id, _attempt, cooked) = checked_run_with_queued_check(
            &dir,
            &mut ledger,
            &mut rt,
            &mut graph,
            "#!/bin/sh\nsleep 60\n",
            CHECKED_FAST,
        );
        graph.execute(&mut ledger).unwrap();
        // the deadline contributes to the poll timeout (<= 200ms)
        let timeout = graph.poll_timeout(std::time::Instant::now()).unwrap();
        assert!(
            timeout <= std::time::Duration::from_millis(200),
            "{timeout:?}"
        );
        // a wake past the deadline kills; `now` is injectable
        graph.kill_expired(std::time::Instant::now() + std::time::Duration::from_secs(1));
        wait_and_reap_checks(&mut graph, &mut ledger);
        let failed = events_named(&ledger, "check.failed");
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].data["timed_out"], true);
        // a timeout is a spent iteration, not a hard stop: attempt 2 exists
        assert_eq!(
            ledger
                .step_attempts(&cooked.run_id, "impl", &anchor_id)
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            ledger.get_bead(&anchor_id).unwrap().unwrap().status,
            "in_progress"
        );
    }

    #[test]
    fn a_missing_check_script_hard_fails_the_anchor_without_burning_budget() {
        let _spawning = crate::daemon::spawn_probe_guard();
        let (dir, mut ledger, mut rt, mut graph) = graph_fixture();
        // CHECKED names verify.sh but we never create it
        let cooked = cook_into(&dir, &mut ledger, "checked", CHECKED);
        settle_graph(&mut ledger, &mut rt, &mut graph);
        let anchor_id = cooked.step_beads["impl"].clone();
        let attempt = ledger
            .step_attempts(&cooked.run_id, "impl", &anchor_id)
            .unwrap()[0]
            .id
            .clone();
        append_close(&mut ledger, &attempt, serde_json::json!({"outcome":"pass"}));
        settle_graph(&mut ledger, &mut rt, &mut graph);
        graph.execute(&mut ledger).unwrap();
        let failed = events_named(&ledger, "check.failed");
        assert_eq!(failed.len(), 1, "exactly one check.failed (Decision 10)");
        assert!(
            failed[0].data["error"]
                .as_str()
                .unwrap()
                .contains("failed to start")
        );
        let close = ledger.close_event_data(&anchor_id).unwrap().unwrap();
        assert_eq!(close["outcome"], "fail");
        assert_eq!(close["final_disposition"], "hard_fail");
        assert_eq!(
            ledger
                .step_attempts(&cooked.run_id, "impl", &anchor_id)
                .unwrap()
                .len(),
            1,
            "no budget loop over a structural problem"
        );
    }

    // ---- Phase 9 Task 7: on_complete fan-out -----------------------------

    const FAN_PARALLEL: &str = "formula = \"fan\"\n\n[requires]\nformula_compiler = \">=2.0.0\"\n\n[[steps]]\nid = \"enumerate\"\ntitle = \"Enumerate\"\n\n[steps.on_complete]\nfor_each = \"output.items\"\nbond = \"child\"\n\n[steps.on_complete.vars]\nname = \"{item.name}\"\nposition = \"{index}\"\n";
    const FAN_SEQUENTIAL: &str = "formula = \"fan\"\n\n[requires]\nformula_compiler = \">=2.0.0\"\n\n[[steps]]\nid = \"enumerate\"\ntitle = \"Enumerate\"\n\n[steps.on_complete]\nfor_each = \"output.items\"\nbond = \"child\"\nsequential = true\n\n[steps.on_complete.vars]\nname = \"{item.name}\"\nposition = \"{index}\"\n";
    const CHILD: &str = "formula = \"child\"\n\n[[steps]]\nid = \"work\"\ntitle = \"Handle {name} at {position}\"\n";

    fn write_bond(dir: &tempfile::TempDir) {
        std::fs::create_dir_all(dir.path().join("formulas")).unwrap();
        std::fs::write(dir.path().join("formulas/child.toml"), CHILD).unwrap();
    }

    /// Cook a fan parent and close its enumerate step pass with `output`.
    fn fan_run(
        dir: &tempfile::TempDir,
        ledger: &mut Ledger,
        rt: &mut super::super::orders::OrdersRuntime,
        graph: &mut GraphRuntime,
        toml: &str,
        output: serde_json::Value,
    ) -> CookedRun {
        write_bond(dir);
        let cooked = cook_into(dir, ledger, "fan", toml);
        settle_graph(ledger, rt, graph);
        append_close(
            ledger,
            &cooked.step_beads["enumerate"],
            serde_json::json!({"outcome":"pass","output": output}),
        );
        settle_graph(ledger, rt, graph);
        cooked
    }

    fn bond_children(ledger: &Ledger, anchor: &str) -> Vec<(usize, BeadRow)> {
        existing_bond_children(ledger, anchor)
            .unwrap()
            .into_iter()
            .collect()
    }

    #[test]
    fn parallel_fanout_cooks_every_item_with_substituted_vars() {
        let (dir, mut ledger, mut rt, mut graph) = graph_fixture();
        let cooked = fan_run(
            &dir,
            &mut ledger,
            &mut rt,
            &mut graph,
            FAN_PARALLEL,
            serde_json::json!({"items":[{"name":"a"},{"name":"b"},{"name":"c"}]}),
        );
        let anchor = cooked.step_beads["enumerate"].clone();
        graph.execute(&mut ledger).unwrap();
        let children = bond_children(&ledger, &anchor);
        assert_eq!(children.len(), 3, "three bonds fan out");
        assert_eq!(
            children.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        // vars substituted into the child step beads
        settle_graph(&mut ledger, &mut rt, &mut graph);
        let mut titles: Vec<String> = Vec::new();
        for (_, root) in &children {
            let m = ledger.run_membership(&root.id).unwrap().unwrap();
            for bead in ledger.run_step_beads(&m.run_id, "work").unwrap() {
                titles.push(bead.title);
            }
        }
        titles.sort();
        assert_eq!(
            titles,
            vec!["Handle a at 0", "Handle b at 1", "Handle c at 2"]
        );
        // the parent run finalizes without waiting for children (Decision 5)
        assert_eq!(events_named(&ledger, "run.finalized").len(), 1);
        // idempotent: re-settle + re-execute cooks nothing more
        let before = ledger.events_range(1, None).unwrap().len();
        settle_graph(&mut ledger, &mut rt, &mut graph);
        graph.execute(&mut ledger).unwrap();
        assert_eq!(ledger.events_range(1, None).unwrap().len(), before);
        assert!(ledger.refold_check().unwrap().drift.is_empty());
    }

    #[test]
    fn sequential_fanout_cooks_lazily_and_chains_on_pass() {
        let (dir, mut ledger, mut rt, mut graph) = graph_fixture();
        let cooked = fan_run(
            &dir,
            &mut ledger,
            &mut rt,
            &mut graph,
            FAN_SEQUENTIAL,
            serde_json::json!({"items":[{"name":"a"},{"name":"b"},{"name":"c"}]}),
        );
        let anchor = cooked.step_beads["enumerate"].clone();
        graph.execute(&mut ledger).unwrap();
        settle_graph(&mut ledger, &mut rt, &mut graph);
        let children = bond_children(&ledger, &anchor);
        assert_eq!(children.len(), 1, "sequential cooks item 0 alone");

        // child 0 passes: its finalization queues the parent fan-out again
        let child0 = &children[0].1;
        let m0 = ledger.run_membership(&child0.id).unwrap().unwrap();
        let step0 = ledger.run_step_beads(&m0.run_id, "work").unwrap()[0]
            .id
            .clone();
        append_close(&mut ledger, &step0, serde_json::json!({"outcome":"pass"}));
        settle_graph(&mut ledger, &mut rt, &mut graph);
        assert_eq!(
            ledger.get_bead(&child0.id).unwrap().unwrap().status,
            "closed",
            "child 0 finalized"
        );
        graph.execute(&mut ledger).unwrap();
        settle_graph(&mut ledger, &mut rt, &mut graph);
        let children = bond_children(&ledger, &anchor);
        assert_eq!(children.len(), 2, "child 1 cooked after child 0 passed");
        // the literal chain edge: child 1's root needs child 0's root
        let child1_created = ledger
            .created_event_data(&children[1].1.id)
            .unwrap()
            .unwrap();
        let needs: Vec<String> = child1_created["needs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_owned())
            .collect();
        assert!(needs.contains(&child0.id), "{needs:?}");

        // child 1 FAILS: the chain halts — no child 2, ever
        let m1 = ledger.run_membership(&children[1].1.id).unwrap().unwrap();
        let step1 = ledger.run_step_beads(&m1.run_id, "work").unwrap()[0]
            .id
            .clone();
        append_close(&mut ledger, &step1, serde_json::json!({"outcome":"fail"}));
        settle_graph(&mut ledger, &mut rt, &mut graph);
        graph.execute(&mut ledger).unwrap();
        settle_graph(&mut ledger, &mut rt, &mut graph);
        graph.execute(&mut ledger).unwrap();
        assert_eq!(
            bond_children(&ledger, &anchor).len(),
            2,
            "a non-pass child halts the chain"
        );
        assert!(ledger.refold_check().unwrap().drift.is_empty());
    }

    #[test]
    fn a_bad_for_each_path_events_a_dispatch_failed_on_the_anchor() {
        let (dir, mut ledger, mut rt, mut graph) = graph_fixture();
        let cooked = fan_run(
            &dir,
            &mut ledger,
            &mut rt,
            &mut graph,
            FAN_PARALLEL,
            serde_json::json!({"wrong": []}),
        );
        let anchor = cooked.step_beads["enumerate"].clone();
        graph.execute(&mut ledger).unwrap();
        let failed = events_named(&ledger, "dispatch.failed");
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].bead.as_deref(), Some(anchor.as_str()));
        assert!(
            failed[0].data["reason"]
                .as_str()
                .unwrap()
                .contains("output.items"),
            "{}",
            failed[0].data
        );
        assert!(bond_children(&ledger, &anchor).is_empty());
    }

    /// Review MEDIUM 1: a check that EXITED 0 before its deadline must be
    /// classified by its real exit status, not misrecorded as timed out
    /// just because the wake arrived late (busy-camp settle). kill_expired
    /// must try_wait before marking/killing.
    #[test]
    fn an_on_time_pass_reaped_after_the_deadline_is_still_a_pass() {
        let _spawning = crate::daemon::spawn_probe_guard();
        let (dir, mut ledger, mut rt, mut graph) = graph_fixture();
        let (anchor_id, attempt, _cooked) = checked_run_with_queued_check(
            &dir,
            &mut ledger,
            &mut rt,
            &mut graph,
            "#!/bin/sh\nexit 0\n",
            CHECKED,
        );
        graph.execute(&mut ledger).unwrap();
        // the check exits 0 well within its declared timeout...
        for check in graph.check_children.values_mut() {
            let _ = check.child.wait();
        }
        // ...but campd is busy and the deadline-enforcement wake arrives
        // AFTER the deadline passed
        graph.kill_expired(std::time::Instant::now() + std::time::Duration::from_secs(3600));
        graph.reap_checks(&mut ledger).unwrap();
        assert_eq!(
            events_named(&ledger, "check.passed").len(),
            1,
            "an on-time exit 0 is a pass, whatever the wake latency"
        );
        assert_eq!(events_named(&ledger, "check.failed").len(), 0);
        let close = ledger.close_event_data(&anchor_id).unwrap().unwrap();
        assert_eq!(close["outcome"], "pass");
        let _ = attempt;
    }

    // ---- Phase 9 Task 8: startup reconciliation ---------------------------

    #[test]
    fn reconcile_requeues_an_interrupted_check_exactly_once() {
        let _spawning = crate::daemon::spawn_probe_guard();
        let (dir, mut ledger, mut rt, mut graph) = graph_fixture();
        let (anchor_id, attempt, _cooked) = checked_run_with_queued_check(
            &dir,
            &mut ledger,
            &mut rt,
            &mut graph,
            "#!/bin/sh\nexit 0\n",
            CHECKED,
        );
        // kill -9 before the queued check ever spawned: a fresh campd life
        // has an empty queue but the same ledger
        let config =
            CampConfig::parse(&std::fs::read_to_string(dir.path().join("camp.toml")).unwrap())
                .unwrap();
        let mut fresh = GraphRuntime::new(dir.path().to_path_buf(), &config);
        fresh.reconcile(&mut ledger).unwrap();
        let queue = fresh.pending_check_queue();
        assert_eq!(queue.len(), 1, "the interrupted check re-queues");
        assert_eq!(queue[0].anchor, anchor_id);
        assert_eq!(queue[0].attempt_bead, attempt);
        assert_eq!(queue[0].attempt_no, 1);
        // the verdict lands; a NEXT restart reconciles nothing
        fresh.execute(&mut ledger).unwrap();
        wait_and_reap_checks(&mut fresh, &mut ledger);
        assert_eq!(
            ledger.get_bead(&anchor_id).unwrap().unwrap().status,
            "closed"
        );
        let mut later = GraphRuntime::new(dir.path().to_path_buf(), &config);
        later.reconcile(&mut ledger).unwrap();
        assert!(
            later.pending_check_queue().is_empty(),
            "verdict landed: nothing owed"
        );
    }

    #[test]
    fn reconcile_requeues_an_incomplete_fanout() {
        let (dir, mut ledger, mut rt, mut graph) = graph_fixture();
        let cooked = fan_run(
            &dir,
            &mut ledger,
            &mut rt,
            &mut graph,
            FAN_PARALLEL,
            serde_json::json!({"items":[{"name":"a"},{"name":"b"},{"name":"c"}]}),
        );
        let anchor = cooked.step_beads["enumerate"].clone();
        // crash before execute: children never cooked
        let config =
            CampConfig::parse(&std::fs::read_to_string(dir.path().join("camp.toml")).unwrap())
                .unwrap();
        let mut fresh = GraphRuntime::new(dir.path().to_path_buf(), &config);
        fresh.reconcile(&mut ledger).unwrap();
        fresh.execute(&mut ledger).unwrap();
        assert_eq!(
            bond_children(&ledger, &anchor).len(),
            3,
            "reconcile + execute completes the lost fan-out"
        );
        // a later restart owes nothing new
        let mut later = GraphRuntime::new(dir.path().to_path_buf(), &config);
        later.reconcile(&mut ledger).unwrap();
        let before = ledger.events_range(1, None).unwrap().len();
        later.execute(&mut ledger).unwrap();
        assert_eq!(ledger.events_range(1, None).unwrap().len(), before);
    }

    /// Review LOW 3: a run whose pinned dir vanished while campd was down
    /// (and with no further ledger events coming) must be dead-ended BY
    /// RECONCILE — evented, never a silent eprintln + NotQuiescent-forever.
    #[test]
    fn reconcile_dead_ends_a_run_whose_dir_vanished() {
        let (dir, mut ledger, mut rt, mut graph) = graph_fixture();
        let cooked = cook_into(&dir, &mut ledger, "two-step", TWO_STEP);
        settle_graph(&mut ledger, &mut rt, &mut graph);
        // the dir vanishes while campd is down
        std::fs::remove_dir_all(
            camp_core::formula::runtime::runs_dir(dir.path()).join(&cooked.run_id),
        )
        .unwrap();
        let config =
            CampConfig::parse(&std::fs::read_to_string(dir.path().join("camp.toml")).unwrap())
                .unwrap();
        let mut fresh = GraphRuntime::new(dir.path().to_path_buf(), &config);
        fresh.reconcile(&mut ledger).unwrap();
        let root = ledger.get_bead(&cooked.root_bead).unwrap().unwrap();
        assert_eq!(
            root.status, "closed",
            "the run must be dead-ended, not skipped"
        );
        assert_eq!(root.outcome.as_deref(), Some("fail"));
        for step in ["a", "b"] {
            assert_eq!(
                ledger
                    .get_bead(&cooked.step_beads[step])
                    .unwrap()
                    .unwrap()
                    .outcome
                    .as_deref(),
                Some("skipped"),
                "step {step}"
            );
        }
        let finalized = events_named(&ledger, "run.finalized");
        assert_eq!(finalized.len(), 1);
        assert_eq!(finalized[0].data["final_disposition"], "hard_fail");
        // idempotent: a second reconcile appends nothing
        let before = ledger.events_range(1, None).unwrap().len();
        let mut again = GraphRuntime::new(dir.path().to_path_buf(), &config);
        again.reconcile(&mut ledger).unwrap();
        assert_eq!(ledger.events_range(1, None).unwrap().len(), before);
        assert!(ledger.refold_check().unwrap().drift.is_empty());
    }

    #[test]
    fn graph_appends_are_idempotent_across_resettles() {
        let (dir, mut ledger, mut rt, mut graph) = graph_fixture();
        cook_into(&dir, &mut ledger, "retry-soft", RETRY_SOFT);
        settle_graph(&mut ledger, &mut rt, &mut graph);
        let before = ledger.events_range(1, None).unwrap().len();
        settle_graph(&mut ledger, &mut rt, &mut graph);
        settle_graph(&mut ledger, &mut rt, &mut graph);
        assert_eq!(
            ledger.events_range(1, None).unwrap().len(),
            before,
            "re-settling an already-settled ledger appends nothing"
        );
    }

    // ---- Phase 11: held-stdin worker lifecycle ---------------------------

    fn test_dispatcher(dir: &std::path::Path) -> Dispatcher {
        Dispatcher::new(
            CampDir {
                root: dir.to_path_buf(),
            },
            CampConfig::parse("[camp]\nname = \"t\"\n").unwrap(),
        )
    }

    /// A live `cat` worker with a held stdin pipe and stdout captured to a
    /// file — the stream-worker stand-in.
    fn held_cat_worker(dir: &std::path::Path, session: &str, bead: &str) -> Worker {
        let _spawning = crate::daemon::spawn_probe_guard();
        let out = std::fs::File::create(dir.join(format!("{}.out", bead))).unwrap();
        let mut child = std::process::Command::new("cat")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::from(out))
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().map(mio::unix::pipe::Sender::from);
        Worker {
            child,
            session: session.to_owned(),
            bead: bead.to_owned(),
            rig: "gc".into(),
            rig_path: dir.to_path_buf(),
            worktree: None,
            end_recorded: false,
            stdin,
            released: None,
            patrol_kill: None,
            kill_reason: None,
        }
    }

    /// ROUND-2 LOW 2: a cap-full patrol respawn must QUEUE and retry when
    /// a worker slot frees — never strand the bead with a false "deferred"
    /// event that nothing acts on. Reachable from the AdoptedPid restart
    /// path, whose non-child kill frees no slot in `children`.
    #[test]
    fn a_cap_full_patrol_respawn_queues_and_retries_when_a_slot_frees() {
        // NOTE: do NOT hold spawn_probe_guard here — `held_cat_worker`
        // acquires it per call, and the guard mutex is non-reentrant, so a
        // test-level hold would self-deadlock. The one fork this test does
        // outside held_cat_worker (converge → /bin/echo) is guarded inline.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("rig")).unwrap();
        // The subject is the respawn queue, not isolation: pin the
        // live-tree opt-out (spec §12) so the plain-dir rig dispatches.
        // Directory agent (umbrella §5.1) — the opt-out lives in agent.toml.
        let dev = root.join("agents/dev");
        std::fs::create_dir_all(&dev).unwrap();
        std::fs::write(dev.join("agent.toml"), "isolation = \"none\"\n").unwrap();
        std::fs::write(dev.join("prompt.md"), "Work.\n").unwrap();
        std::fs::write(
            root.join("camp.toml"),
            format!(
                "[camp]\nname = \"t\"\n\n[[rigs]]\nname = \"gc\"\npath = \"{}\"\nprefix = \"gc\"\n\n\
                 [dispatch]\nmax_workers = 1\ncommand = \"/bin/echo\"\ndefault_agent = \"dev\"\n\n\
                 [agent_defaults]\ntools = [\"Read\"]\n",
                root.join("rig").display()
            ),
        )
        .unwrap();
        let config = CampConfig::load(&root.join("camp.toml")).unwrap();
        let mut ledger = Ledger::open(&root.join("camp.db")).unwrap();
        // gc-9: created, then EVER-SESSIONED (a patrol-killed worker) so
        // `dispatchable_beads` excludes it — only the respawn queue can
        // re-hook it. The crash releases it back to open.
        ledger
            .append(EventInput {
                kind: EventType::BeadCreated,
                rig: Some("gc".into()),
                actor: "test".into(),
                bead: Some("gc-9".into()),
                data: serde_json::json!({"title": "respawn me"}),
            })
            .unwrap();
        ledger
            .append(EventInput {
                kind: EventType::SessionWoke,
                rig: Some("gc".into()),
                actor: "campd".into(),
                bead: Some("gc-9".into()),
                data: serde_json::json!({"name": "t/dev/1", "agent": "dev", "bead": "gc-9"}),
            })
            .unwrap();
        ledger
            .append(EventInput {
                kind: EventType::BeadClaimed,
                rig: Some("gc".into()),
                actor: "cli".into(),
                bead: Some("gc-9".into()),
                data: serde_json::json!({"session": "t/dev/1"}),
            })
            .unwrap();
        ledger
            .append(EventInput {
                kind: EventType::SessionCrashed,
                rig: Some("gc".into()),
                actor: "campd".into(),
                bead: None,
                data: serde_json::json!({"name": "t/dev/1", "reason": "patrol restart"}),
            })
            .unwrap();

        let gc9_wokes = |l: &Ledger| -> usize {
            l.events_of_type(EventType::SessionWoke)
                .unwrap()
                .iter()
                .filter(|e| e.data["bead"] == "gc-9")
                .count()
        };
        assert_eq!(gc9_wokes(&ledger), 1, "just the setup woke so far");

        let mut dispatcher = Dispatcher::new(
            CampDir {
                root: root.to_path_buf(),
            },
            config,
        );
        // an ADOPTED restart frees no slot, so children stays full while
        // the respawn is attempted — the exact cap-full path.
        let occupant = held_cat_worker(root, "t/dev/occ", "gc-other");
        let occupant_pid = occupant.child.id();
        dispatcher.children.insert(occupant_pid, occupant);

        // cap full → queued and evented ONCE, truthfully (will retry)
        dispatcher.dispatch_bead(&mut ledger, "gc-9").unwrap();
        assert_eq!(gc9_wokes(&ledger), 1, "no dispatch while capped");
        assert_eq!(count(&ledger, "dispatch.failed"), 1);
        let failed = ledger.events_of_type(EventType::DispatchFailed).unwrap();
        assert!(
            failed[0].data["reason"].as_str().unwrap().contains("retry"),
            "the deferral must be truthful: {}",
            failed[0].data["reason"]
        );
        // issue #83 review F1: the deferral is campd's OWN pending retry —
        // it must carry the shared prefix and must NOT count as `stuck`
        // (stuck promises `camp retry` can fix it; here it cannot).
        assert!(
            camp_core::readiness::is_deferred_dispatch_failure(
                failed[0].data["reason"].as_str().unwrap()
            ),
            "the deferral reason must carry DEFERRED_DISPATCH_PREFIX: {}",
            failed[0].data["reason"]
        );
        assert_eq!(
            ledger.status_summary().unwrap().stuck,
            0,
            "a cap-deferred bead is never counted stuck"
        );
        // a second attempt while still capped must NOT re-event
        dispatcher.dispatch_bead(&mut ledger, "gc-9").unwrap();
        assert_eq!(
            count(&ledger, "dispatch.failed"),
            1,
            "no duplicate deferral event"
        );

        // free the slot (as a reap would) and converge: the queue drains
        {
            let w = dispatcher.children.get_mut(&occupant_pid).unwrap();
            let _ = w.child.kill();
            let _ = w.child.wait();
        }
        dispatcher.children.remove(&occupant_pid);
        {
            // converge forks /bin/echo for the respawn — serialize it
            // against the socket-probe tests (the guard is released before
            // any nested held_cat call, so no re-entrancy).
            let _spawning = crate::daemon::spawn_probe_guard();
            dispatcher.converge(&mut ledger).unwrap();
        }
        assert_eq!(
            gc9_wokes(&ledger),
            2,
            "the freed slot re-hooks the queued respawn"
        );
    }

    #[test]
    fn released_worker_reaps_as_stopped_with_the_reason() {
        let (dir, mut ledger) = temp_ledger();
        wake_session(&mut ledger, "t/dev/1");
        ledger
            .append(EventInput {
                kind: EventType::BeadCreated,
                rig: Some("gc".into()),
                actor: "test".into(),
                bead: Some("gc-1".into()),
                data: serde_json::json!({"title": "t"}),
            })
            .unwrap();
        ledger
            .append(EventInput {
                kind: EventType::BeadClaimed,
                rig: Some("gc".into()),
                actor: "cli".into(),
                bead: Some("gc-1".into()),
                data: serde_json::json!({"session": "t/dev/1"}),
            })
            .unwrap();

        let mut dispatcher = test_dispatcher(dir.path());
        let mut worker = held_cat_worker(dir.path(), "t/dev/1", "gc-1");
        worker.stdin = None; // released: pipe already dropped
        worker.released = Some("released after bead close".into());
        worker.child.kill().unwrap();
        worker.child.wait().unwrap(); // try_wait re-returns the status
        dispatcher.children.insert(worker.child.id(), worker);

        dispatcher.reap(&mut ledger).unwrap();
        assert_eq!(count(&ledger, "session.stopped"), 1);
        assert_eq!(count(&ledger, "session.crashed"), 0, "released ≠ crashed");
        let events = ledger.events_range(1, None).unwrap();
        let stopped = events
            .iter()
            .find(|e| e.kind.as_str() == "session.stopped")
            .unwrap();
        assert!(
            stopped.data["reason"]
                .as_str()
                .unwrap()
                .contains("released"),
            "{}",
            stopped.data
        );
        // stopped (not crashed) leaves the claim in place
        let bead = ledger.get_bead("gc-1").unwrap().unwrap();
        assert_eq!(bead.status, "in_progress");
    }

    #[test]
    fn patrol_killed_worker_reaps_as_crashed_with_cause_seq() {
        let (dir, mut ledger) = temp_ledger();
        wake_session(&mut ledger, "t/dev/1");
        let mut dispatcher = test_dispatcher(dir.path());
        let worker = held_cat_worker(dir.path(), "t/dev/1", "gc-1");
        let pid = worker.child.id();
        dispatcher.children.insert(pid, worker);

        assert!(dispatcher.kill_worker("t/dev/1", 41));
        assert!(!dispatcher.kill_worker("ghost", 41), "unknown session");
        // the kill lands as SIGKILL; wait for the exit so try_wait sees it
        loop {
            if dispatcher
                .children
                .get_mut(&pid)
                .unwrap()
                .child
                .try_wait()
                .unwrap()
                .is_some()
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        dispatcher.reap(&mut ledger).unwrap();
        let events = ledger.events_range(1, None).unwrap();
        let crashed = events
            .iter()
            .find(|e| e.kind.as_str() == "session.crashed")
            .expect("patrol kill must reap as crashed");
        assert_eq!(crashed.data["signal"], 9);
        assert_eq!(crashed.data["reason"], "patrol restart");
        assert_eq!(crashed.data["cause_seq"], 41);
        assert!(dispatcher.children.is_empty());
    }

    /// cp-0 §2.3 / amendment fix 2: `kill_worker_with_reason` (the
    /// max_stream_bytes ceiling kill) marks the worker with a custom reason
    /// and cause_seq; the reap classifies the exit as `session.crashed`
    /// carrying BOTH (so the ledger names the cap — invariant 3, and the
    /// `patrol restart` prefix lets patrol::observe queue a Respawn — fix 6).
    #[test]
    fn cap_breach_kill_worker_with_reason_reaps_as_crashed_with_the_named_reason() {
        let (dir, mut ledger) = temp_ledger();
        wake_session(&mut ledger, "t/dev/1");
        let mut dispatcher = test_dispatcher(dir.path());
        let worker = held_cat_worker(dir.path(), "t/dev/1", "gc-1");
        let pid = worker.child.id();
        dispatcher.children.insert(pid, worker);

        let reason = "patrol restart: stream cap exceeded max_stream_bytes".to_owned();
        assert!(
            dispatcher
                .kill_worker_with_reason("t/dev/1", 57, reason.clone())
                .unwrap(),
            "a live worker is killed"
        );
        // review fix 3: an unknown session returns Ok(false) — the kill was
        // NOT delivered. The event loop must act on this (durable fault
        // event + unregister), never discard it.
        assert!(
            !dispatcher
                .kill_worker_with_reason("ghost", 57, reason)
                .unwrap(),
            "no live worker for the session => Ok(false), the kill was not delivered"
        );
        // SIGKILL lands; wait for the exit so try_wait sees it.
        loop {
            if dispatcher
                .children
                .get_mut(&pid)
                .unwrap()
                .child
                .try_wait()
                .unwrap()
                .is_some()
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        dispatcher.reap(&mut ledger).unwrap();
        let events = ledger.events_range(1, None).unwrap();
        let crashed = events
            .iter()
            .find(|e| e.kind.as_str() == "session.crashed")
            .expect("cap-breach kill must reap as crashed");
        assert_eq!(crashed.data["signal"], 9);
        assert_eq!(
            crashed.data["reason"], "patrol restart: stream cap exceeded max_stream_bytes",
            "the reap carries the custom kill reason (names the cap)"
        );
        assert_eq!(
            crashed.data["cause_seq"], 57,
            "cause_seq points at stream_capped"
        );
        assert!(dispatcher.children.is_empty());
    }

    #[test]
    fn nudge_via_stdin_writes_one_message_line_or_reports_no_pipe() {
        let (dir, mut ledger) = temp_ledger();
        wake_session(&mut ledger, "t/dev/1");
        let mut dispatcher = test_dispatcher(dir.path());
        let worker = held_cat_worker(dir.path(), "t/dev/1", "gc-1");
        let pid = worker.child.id();
        dispatcher.children.insert(pid, worker);

        assert!(dispatcher.is_child("t/dev/1"));
        assert!(!dispatcher.is_child("ghost"));
        assert!(matches!(
            dispatcher.nudge_via_stdin("t/dev/1", "status?"),
            NudgeOutcome::Delivered
        ));
        // drop the pipe (release) so cat exits and we can read the capture
        assert_eq!(
            dispatcher.release_worker("gc-1", "done"),
            Some("t/dev/1".to_owned())
        );
        let worker = dispatcher.children.get_mut(&pid).unwrap();
        worker.child.wait().unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("gc-1.out")).unwrap(),
            crate::daemon::spawn::user_message("status?"),
            "the wire carries exactly one user_message line"
        );

        // released pipe: no pipe to nudge
        assert!(matches!(
            dispatcher.nudge_via_stdin("t/dev/1", "again?"),
            NudgeOutcome::NoPipe
        ));
        // unknown session: NoPipe as well (the resume path takes over)
        assert!(matches!(
            dispatcher.nudge_via_stdin("ghost", "x"),
            NudgeOutcome::NoPipe
        ));
    }

    #[test]
    fn a_dead_reader_nudge_fails_loudly() {
        let (dir, _ledger) = temp_ledger();
        let mut dispatcher = test_dispatcher(dir.path());
        let mut worker = held_cat_worker(dir.path(), "t/dev/1", "gc-1");
        // The dead reader: killed and reaped, but retained ONLY as a
        // plausible dead-worker `child` handle for the Worker row — the
        // EPIPE the assertion observes comes from the write-shut socket
        // installed below, not from this cat's pipe.
        worker.child.kill().unwrap();
        worker.child.wait().unwrap();
        // A killed reader's pipe cannot make the EPIPE deterministic: a
        // sibling test mid-Command::spawn (forked, execve pending)
        // transiently inherits a copy of every fd in this process —
        // including that pipe's read end — which keeps writes succeeding
        // and flaked this test under parallel load (issue #44, same fd
        // physics as SPAWN_PROBE_LOCK's doc). shutdown(2) is a property of
        // the socket OBJECT, not of fd-table entries, so no inherited copy
        // can hold the write side open: with a write-shut socket as the
        // held stdin, the very FIRST nudge must surface the kernel's EPIPE
        // as Failed. (Peer-side shutdown is not enough — macOS absorbs it.)
        let (ours, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
        ours.shutdown(std::net::Shutdown::Write).unwrap();
        worker.stdin = Some(mio::unix::pipe::Sender::from(
            std::process::ChildStdin::from(std::os::fd::OwnedFd::from(ours)),
        ));
        dispatcher.children.insert(worker.child.id(), worker);
        let outcome = dispatcher.nudge_via_stdin("t/dev/1", "status?");
        assert!(
            matches!(outcome, NudgeOutcome::Failed(_)),
            "a broken pipe must surface as Failed: {outcome:?}"
        );
    }

    #[test]
    fn release_worker_drops_stdin_and_names_the_session() {
        let (dir, _ledger) = temp_ledger();
        let mut dispatcher = test_dispatcher(dir.path());
        let worker = held_cat_worker(dir.path(), "t/dev/1", "gc-1");
        let pid = worker.child.id();
        dispatcher.children.insert(pid, worker);

        assert_eq!(
            dispatcher.release_worker("gc-1", "released after bead close"),
            Some("t/dev/1".to_owned())
        );
        {
            let worker = dispatcher.children.get_mut(&pid).unwrap();
            assert!(worker.stdin.is_none(), "the pipe is dropped (EOF)");
            assert_eq!(
                worker.released.as_deref(),
                Some("released after bead close")
            );
            // cat exits on EOF
            assert!(worker.child.wait().unwrap().success());
        }
        // idempotent: a second release finds nothing to do
        assert_eq!(dispatcher.release_worker("gc-1", "again"), None);
        assert_eq!(dispatcher.release_worker("gc-999", "x"), None);
    }

    #[test]
    fn aux_children_reap_without_session_events_and_event_failures() {
        let (dir, mut ledger) = temp_ledger();
        let mut dispatcher = test_dispatcher(dir.path());
        {
            let _spawning = crate::daemon::spawn_probe_guard();
            let mut ok = std::process::Command::new("true");
            ok.stdin(std::process::Stdio::null());
            dispatcher.spawn_aux("t/dev/1", "nudge-resume", ok).unwrap();
            let mut bad = std::process::Command::new("false");
            bad.stdin(std::process::Stdio::null());
            dispatcher
                .spawn_aux("t/dev/2", "nudge-resume", bad)
                .unwrap();
        }
        // wait for both to exit so one reap sees them
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !dispatcher.aux_done() {
            assert!(std::time::Instant::now() < deadline, "aux children hung");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        dispatcher.reap(&mut ledger).unwrap();
        let events = ledger.events_range(1, None).unwrap();
        assert_eq!(count(&ledger, "session.stopped"), 0, "aux ≠ session end");
        assert_eq!(count(&ledger, "session.crashed"), 0);
        let degraded: Vec<_> = events
            .iter()
            .filter(|e| e.kind.as_str() == "patrol.degraded")
            .collect();
        assert_eq!(degraded.len(), 1, "only the failing aux child events");
        assert_eq!(degraded[0].data["session"], "t/dev/2");
        assert!(
            degraded[0].data["error"]
                .as_str()
                .unwrap()
                .contains("nudge-resume"),
            "{}",
            degraded[0].data
        );
    }
    // ======== cp-1 Task 5: write_control — the write half ==================

    /// cp-1 (§2): a control line goes into the SAME held stdin pipe a turn does
    /// — campd already holds it, so there is no new transport to build.
    #[test]
    fn write_control_delivers_into_the_held_stdin_pipe() {
        let (dir, mut ledger) = temp_ledger();
        wake_session(&mut ledger, "t/dev/1");
        let mut dispatcher = test_dispatcher(dir.path());
        dispatcher.test_insert_held_cat(dir.path(), "t/dev/1", "gc-1");

        let line = crate::daemon::control::ParentMessage::Interrupt {
            request_id: "camp-1".into(),
        }
        .to_line()
        .unwrap();
        assert!(matches!(
            dispatcher.write_control("t/dev/1", &line),
            ControlWrite::Delivered
        ));

        // An unknown session has no pipe. Unlike a turn, an interrupt has NO
        // resume path — so this is a caller-visible FAILURE, not a degrade.
        assert!(matches!(
            dispatcher.write_control("ghost", &line),
            ControlWrite::NoPipe
        ));

        // Drop the pipe (the release path) so `cat` exits, then read what it saw.
        for w in dispatcher.children.values_mut() {
            w.stdin = None;
        }
        for w in dispatcher.children.values_mut() {
            let _ = w.child.wait();
        }
        let captured = std::fs::read_to_string(dir.path().join("gc-1.out")).unwrap();
        assert_eq!(
            captured, line,
            "the worker must see EXACTLY the control line camp built — the bytes \
             are pinned by the fixtures, and this is the path they travel"
        );
    }

    /// PR #51's finding-2 wedge shape, applied to the control write.
    ///
    /// THIS IS THE WHOLE JUSTIFICATION FOR THE METHOD EXISTING. `session.interrupt`
    /// is operator-triggerable over the socket, and an UNBOUNDED blocking write
    /// into the full pipe of a worker that has stopped reading would wedge
    /// campd's single-threaded event loop — no dispatch, no SIGCHLD reaping —
    /// until it drained. It must fail, and fail FAST.
    #[test]
    fn write_control_is_bounded_and_drops_the_torn_pipe() {
        let (dir, mut ledger) = temp_ledger();
        wake_session(&mut ledger, "t/dev/1");
        let mut dispatcher = test_dispatcher(dir.path());
        // A worker that NEVER reads its pipe.
        dispatcher.test_insert_held_sleeper(dir.path(), "t/dev/1", "gc-1");

        // Far more than any pipe buffer will take.
        let big = format!("{}\n", "x".repeat(2 * 1024 * 1024));
        let started = std::time::Instant::now();
        let outcome = dispatcher.write_control("t/dev/1", &big);
        let elapsed = started.elapsed();

        assert!(
            matches!(outcome, ControlWrite::Failed(_)),
            "a full pipe must FAIL the control write, never block campd: {outcome:?}"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(10),
            "the write must be BOUNDED — it took {elapsed:?}. An unbounded write \
             here is issue #55's wedge class, on the event loop"
        );

        // The pipe may hold a TORN partial line, so it is dropped: no later
        // turn or control message may interleave garbage behind it.
        assert!(
            matches!(
                dispatcher.write_control("t/dev/1", "{}\n"),
                ControlWrite::NoPipe
            ),
            "after a failed write the torn pipe must be DROPPED"
        );
    }
}

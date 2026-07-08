//! The dispatcher (spec §7.3, §8.3, §8.4): on every wake, converge the
//! ledger's dispatchable set onto live worker children, up to
//! [dispatch].max_workers. Query-driven from ledger truth (Phase 8 plan
//! decision B) — crash-only, no in-memory queue to lose. Every failure
//! lands in the ledger (`dispatch.failed`, `session.crashed`), never in a
//! void: campd has no caller (invariant 5).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::ExitStatus;

use anyhow::Result;
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
    /// Beads that failed to dispatch this campd lifetime (plan decision
    /// F): one dispatch.failed each, retried once per restart (crash-only).
    failed: HashSet<String>,
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
    /// must never dangle live (plan decision F).
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
                        end_recorded: false,
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
    pub fn reap(&mut self, ledger: &mut Ledger) -> Result<(), ReapFailure> {
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
                let (kind, exit_code, signal) = classify(status);
                let mut data = serde_json::json!({ "name": worker.session });
                if let Some(code) = exit_code {
                    data["exit_code"] = serde_json::json!(code);
                }
                if let Some(sig) = signal {
                    data["signal"] = serde_json::json!(sig);
                }
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
                Self::dispose_worktree(ledger, worker).map_err(|error| ReapFailure {
                    retryable: true,
                    error,
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
    fn dispose_worktree(ledger: &mut Ledger, worker: &Worker) -> Result<()> {
        let Some(worktree) = &worker.worktree else {
            return Ok(());
        };
        let closed_pass = ledger
            .get_bead(&worker.bead)?
            .is_some_and(|b| b.status == "closed" && b.outcome.as_deref() == Some("pass"));
        let (kind, data) = if closed_pass {
            let removal = if worktree.exists() {
                spawn::remove_worktree(&worker.rig_path, worktree)
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
    /// rig name -> path snapshot (check-script cwd, Task 6) — same
    /// campd-start freshness as the Dispatcher's config.
    #[allow(dead_code)] // consumed by the check executor (Task 6)
    rig_paths: HashMap<String, PathBuf>,
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

impl GraphRuntime {
    pub fn new(camp_root: PathBuf, config: &camp_core::config::CampConfig) -> GraphRuntime {
        GraphRuntime {
            camp_root,
            rig_paths: config
                .rigs
                .iter()
                .map(|r| (r.name.clone(), r.path.clone()))
                .collect(),
            runs: HashMap::new(),
            pending_checks: Vec::new(),
            pending_fanouts: Vec::new(),
            check_children: HashMap::new(),
        }
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
        let Some(rig_path) = self.rig_paths.get(&ctx.rig).cloned() else {
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
                check.timed_out = true;
                // a child that already exited is fine — the reap classifies
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
    fn dead_end_run(
        &mut self,
        conn: &Connection,
        now: &str,
        run_id: &str,
        cause_seq: i64,
    ) -> Result<(), CoreError> {
        let beads = flow::run_bead_ids(conn, run_id)?;
        let root = beads.iter().find(|(_, step)| step.is_none());
        let Some((root_id, _)) = root else {
            return Ok(()); // no root: nothing to finalize
        };
        let Some(root_row) = camp_core::readiness::get_bead(conn, root_id)? else {
            return Ok(());
        };
        if root_row.status == "closed" {
            return Ok(()); // already dead-ended
        }
        let mut skipped: Vec<String> = Vec::new();
        for (id, step_id) in &beads {
            let Some(step_id) = step_id else { continue };
            let Some(row) = camp_core::readiness::get_bead(conn, id)? else {
                continue;
            };
            if row.status != "closed" {
                Ledger::append_on(
                    conn,
                    now,
                    EventInput {
                        kind: EventType::BeadClosed,
                        rig: Some(row.rig.clone()),
                        actor: "campd".into(),
                        bead: Some(id.clone()),
                        data: serde_json::json!({
                            "outcome": "skipped",
                            "reason": "run dir unreadable; the run cannot advance",
                        }),
                    },
                )?;
                if !skipped.contains(step_id) {
                    skipped.push(step_id.clone());
                }
            }
        }
        Ledger::append_on(
            conn,
            now,
            EventInput {
                kind: EventType::BeadClosed,
                rig: Some(root_row.rig.clone()),
                actor: "campd".into(),
                bead: Some(root_id.clone()),
                data: serde_json::json!({
                    "outcome": "fail",
                    "reason": "run dir unreadable; the run cannot advance",
                }),
            },
        )?;
        Ledger::append_on(
            conn,
            now,
            EventInput {
                kind: EventType::RunFinalized,
                rig: Some(root_row.rig.clone()),
                actor: "campd".into(),
                bead: Some(root_id.clone()),
                data: serde_json::json!({
                    "run_id": run_id,
                    "root": root_id,
                    "outcome": "fail",
                    "final_disposition": "hard_fail",
                    "cause_seq": cause_seq,
                    "soft_failed": [],
                    "skipped": skipped,
                }),
            },
        )?;
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
        super::super::orders::settle(ledger, &mut readiness, rt, &clock, graph).unwrap();
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
}

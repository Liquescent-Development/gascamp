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
}

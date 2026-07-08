//! The health patrol runtime (spec §10, Phase 11): transcript watches
//! (notify → self-pipe, the config-watch mold), per-worker stall timers
//! (heap-sourced poll timeout, camp-core patrol::timers), ledger-event
//! observation on the campd processing path, durable stall declaration
//! (agent.stalled BEFORE any action — the declare_cron_fires mold), the
//! nudge/restart/release executors, and adoption.
//!
//! Patrol tracks a session iff its registry row carries BOTH a transcript
//! path and a bead: a session without a bead is not a worker (spec §10
//! "one armed timer per *active* worker"), and agent.stalled names the
//! bead by contract. Sessions whose session.woke actor is not "campd" are
//! annotate-only: agent.stalled + re-arm, never nudge/kill (spec §10:
//! never kill a session in the user's TUI).
//!
//! Patrol config is read at campd start; hot reload does not re-arm
//! patrol (plan Decision L).

// STAGING ALLOW: the event loop consumes this module in plan Task 11.13;
// this attribute is REMOVED there (the Task 11.16 gates run without it).
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, Result};
use camp_core::Seq;
use camp_core::config::CampConfig;
use camp_core::event::{Event, EventInput, EventType};
use camp_core::ledger::Ledger;
use camp_core::pack;
use camp_core::patrol::timers::{StallFire, StallTimers, TimerKind};
use camp_core::patrol::{Ladder, LadderAction, PatrolConfig, parse_duration};
use jiff::{SignedDuration, Timestamp};

/// Who answers for this session's process (plan Decisions E/F/K).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owned {
    /// A live child of THIS campd: full ladder via the dispatcher.
    Child,
    /// Adopted from a previous campd life at this observed pid: full
    /// ladder via probe-verified non-child kills (round-1 blocker 2:
    /// restarts re-probe before killing).
    AdoptedPid(i64),
    /// Hook-registered (attended): agent.stalled + re-arm ONLY.
    Annotate,
}

/// Everything patrol keeps about one tracked worker session.
#[derive(Debug, Clone)]
struct Tracked {
    bead: String,
    agent: String,
    rig: Option<String>,
    claude_session_id: Option<String>,
    transcript_path: PathBuf,
    worktree: Option<PathBuf>,
    owned: Owned,
    /// The resolved base threshold (agent override or camp default);
    /// the ladder's backoff scales it per restart.
    base_threshold: Option<SignedDuration>, // resolved at apply_tracking
    /// Agent stall_after string, resolved lazily OUTSIDE the cursor txn.
    watch_registered: bool,
}

/// Shared with the notify callback thread (the orders-watch mold, plus a
/// touched-path set so the loop knows which timers to reset).
#[derive(Debug, Default)]
pub struct WatchFilter {
    pub registered: HashSet<PathBuf>,
    pub touched: HashSet<PathBuf>,
    pub error: Option<String>,
}

/// Tracking mutations queued by `observe` (inside the cursor txn,
/// memory-only) and applied by `apply_tracking` (outside it — notify and
/// agent-file I/O live there).
#[derive(Debug)]
enum TrackOp {
    Track { session: String, tracked: Tracked },
    Untrack { session: String },
}

/// Ladder actions queued by `declare_stalls` (after the durable
/// agent.stalled append) and release work queued by `observe`; executed
/// by `execute_pending` (Task 11.11).
#[derive(Debug, PartialEq)]
pub(super) enum PendingAction {
    Nudge { session: String, cause_seq: Seq },
    Restart { session: String, cause_seq: Seq },
    Release { bead: String },
    KillReleased { session: String },
}

pub struct PatrolRuntime {
    config: PatrolConfig,
    camp_config: CampConfig,
    timers: StallTimers,
    ladder: Ladder,
    tracked: HashMap<String, Tracked>,
    filter: Arc<Mutex<WatchFilter>>,
    /// Installed by daemon::run (fail-fast at startup); `None` only in
    /// unit tests, which drive the filter directly.
    watcher: Option<notify::RecommendedWatcher>,
    /// Ref-counted watched transcript PARENT dirs.
    watched_dirs: HashMap<PathBuf, usize>,
    path_to_session: HashMap<PathBuf, String>,
    track_ops: Vec<TrackOp>,
    pending: Vec<PendingAction>,
    /// Sessions with ledger-observed activity awaiting a timer reset
    /// (applied with an explicit `now` at apply_tracking).
    activity: HashSet<String>,
}

/// A poisoned mutex still yields its data (the orders-watch precedent):
/// the callback holds the lock only for inserts, and campd must not die
/// over a poisoned filter.
fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// The notify callback body (runs on the watcher's thread): a hit on a
/// REGISTERED transcript path records the touch and wakes the loop; an
/// error is stored for its durable patrol.degraded. A full pipe is fine —
/// the signal coalesces.
pub(super) fn on_watch_event(
    result: notify::Result<notify::Event>,
    sender: Option<&mio::unix::pipe::Sender>,
    filter: &Mutex<WatchFilter>,
) {
    use std::io::Write as _;
    let signal = match result {
        Ok(event) => {
            let mut filter = lock_unpoisoned(filter);
            let mut hit = false;
            for path in &event.paths {
                if filter.registered.contains(path) {
                    filter.touched.insert(path.clone());
                    hit = true;
                }
            }
            hit
        }
        Err(e) => {
            lock_unpoisoned(filter).error = Some(format!("{e}"));
            true
        }
    };
    if signal && let Some(sender) = sender {
        let _ = (&*sender).write(&[1]);
    }
}

/// The canonical duration string for agent.stalled data: parseable by
/// `patrol::parse_duration`, unambiguous ("600s", "1500ms").
fn threshold_string(d: SignedDuration) -> String {
    let ms = d.as_millis();
    if ms % 1000 == 0 {
        format!("{}s", ms / 1000)
    } else {
        format!("{ms}ms")
    }
}

impl PatrolRuntime {
    pub fn new(config: PatrolConfig, camp_config: &CampConfig) -> PatrolRuntime {
        PatrolRuntime {
            ladder: Ladder::new(config.restart_budget),
            config,
            camp_config: camp_config.clone(),
            timers: StallTimers::new(),
            tracked: HashMap::new(),
            filter: Arc::new(Mutex::new(WatchFilter::default())),
            watcher: None,
            watched_dirs: HashMap::new(),
            path_to_session: HashMap::new(),
            track_ops: Vec::new(),
            pending: Vec::new(),
            activity: HashSet::new(),
        }
    }

    /// The slot the notify callback closure captures.
    pub fn filter_slot(&self) -> Arc<Mutex<WatchFilter>> {
        Arc::clone(&self.filter)
    }

    /// daemon::run installs the real watcher before the loop (fail fast
    /// at startup); unit tests skip it and drive the filter directly.
    pub fn set_watcher(&mut self, watcher: notify::RecommendedWatcher) {
        self.watcher = Some(watcher);
    }

    pub fn poll_timeout(&self, now: Timestamp) -> Option<Duration> {
        self.timers.poll_timeout(now)
    }

    pub fn fire_due(&mut self, now: Timestamp) -> Vec<StallFire> {
        self.timers.fire_due(now)
    }

    /// Observe one committed event (inside the cursor transaction —
    /// MEMORY-ONLY, no I/O). Exclusive dispatch per Decision J(ii): the
    /// lifecycle kinds return before reset matching, and campd-actored
    /// events never reset (J(i) — patrol's own declarations are not
    /// worker activity; round-1 blocker 1).
    pub fn observe(&mut self, event: &Event) {
        match event.kind {
            EventType::SessionWoke => {
                self.observe_woke(event);
                return;
            }
            EventType::SessionStopped | EventType::SessionCrashed => {
                if let Some(name) = event.data["name"].as_str() {
                    self.track_ops.push(TrackOp::Untrack {
                        session: name.to_owned(),
                    });
                }
                return;
            }
            EventType::BeadClosed => {
                if let Some(bead) = event.bead.as_deref()
                    && self.tracked.values().any(|t| t.bead == bead)
                {
                    self.ladder.forget(bead);
                    self.pending.push(PendingAction::Release {
                        bead: bead.to_owned(),
                    });
                }
                return;
            }
            _ => {}
        }
        if event.actor == "campd" {
            return; // J(i): campd bookkeeping is never worker activity
        }
        let data_session = event.data["session"].as_str();
        let sessions: Vec<String> = self
            .tracked
            .iter()
            .filter(|(name, t)| {
                event.bead.as_deref() == Some(t.bead.as_str())
                    || event.actor == **name
                    || data_session == Some(name.as_str())
            })
            .map(|(name, _)| name.clone())
            .collect();
        for session in sessions {
            if let Some(t) = self.tracked.get(&session) {
                self.ladder.on_activity(&t.bead.clone());
            }
            self.activity.insert(session);
        }
    }

    fn observe_woke(&mut self, event: &Event) {
        let data = &event.data;
        let (Some(name), Some(agent)) = (data["name"].as_str(), data["agent"].as_str()) else {
            return; // the fold validated shape; belt-and-braces
        };
        // Patrol tracks workers only: transcript path AND bead required
        // (module docs; agent.stalled names the bead by contract).
        let (Some(transcript), Some(bead)) =
            (data["transcript_path"].as_str(), data["bead"].as_str())
        else {
            return;
        };
        let owned = if event.actor == "campd" {
            Owned::Child
        } else {
            Owned::Annotate
        };
        self.track_ops.push(TrackOp::Track {
            session: name.to_owned(),
            tracked: Tracked {
                bead: bead.to_owned(),
                agent: agent.to_owned(),
                rig: data["rig"].as_str().map(str::to_owned),
                claude_session_id: data["claude_session_id"].as_str().map(str::to_owned),
                transcript_path: PathBuf::from(transcript),
                worktree: data["worktree"].as_str().map(PathBuf::from),
                owned,
                base_threshold: None,
                watch_registered: false,
            },
        });
    }

    /// Re-arm a tracked session for adoption (Decision F): the caller
    /// verified the process alive at `pid`; the timer starts fresh
    /// (restart grace) and later restarts go through the probe-first
    /// non-child path.
    fn adopt_track(&mut self, session: &str, mut tracked: Tracked, pid: i64) {
        if tracked.owned == Owned::Child {
            tracked.owned = Owned::AdoptedPid(pid);
        }
        self.track_ops.push(TrackOp::Track {
            session: session.to_owned(),
            tracked,
        });
    }

    /// Apply queued tracking (notify watches + agent-file threshold
    /// resolution + timer arm/disarm) and ledger-observed activity resets.
    /// Runs OUTSIDE the cursor transaction. A notify error is a durable
    /// patrol.degraded, never a silent skip.
    pub fn apply_tracking(&mut self, ledger: &mut Ledger, now: Timestamp) -> Result<()> {
        let ops = std::mem::take(&mut self.track_ops);
        for op in ops {
            match op {
                TrackOp::Track {
                    session,
                    mut tracked,
                } => {
                    // Threshold: agent stall_after override, else camp
                    // default. A resolution failure on a campd-owned
                    // worker is an anomaly worth a durable event; for
                    // annotate rows the default is simply the answer.
                    let base = match pack::resolve_agent(&self.camp_config, &tracked.agent) {
                        Ok(def) => match def.stall_after.as_deref() {
                            Some(s) => parse_duration(s).unwrap_or(self.config.stall_after),
                            None => self.config.stall_after,
                        },
                        Err(e) => {
                            if tracked.owned == Owned::Child {
                                ledger.append(EventInput {
                                    kind: EventType::PatrolDegraded,
                                    rig: None,
                                    actor: "campd".into(),
                                    bead: None,
                                    data: serde_json::json!({
                                        "error": format!(
                                            "stall threshold fell back to the camp default: {e}"
                                        ),
                                        "session": session,
                                    }),
                                })?;
                            }
                            self.config.stall_after
                        }
                    };
                    tracked.base_threshold = Some(base);
                    self.watch_transcript(ledger, &session, &mut tracked)?;
                    let effective = self.ladder.effective_threshold(&tracked.bead, base);
                    self.timers.arm(&session, TimerKind::Stall, effective, now);
                    self.tracked.insert(session, tracked);
                }
                TrackOp::Untrack { session } => {
                    self.timers.disarm(&session);
                    if let Some(tracked) = self.tracked.remove(&session) {
                        self.unwatch_transcript(&tracked);
                    }
                }
            }
        }
        for session in std::mem::take(&mut self.activity) {
            self.timers.reset(&session, now);
        }
        Ok(())
    }

    fn watch_transcript(
        &mut self,
        ledger: &mut Ledger,
        session: &str,
        tracked: &mut Tracked,
    ) -> Result<()> {
        let parent = tracked
            .transcript_path
            .parent()
            .context("transcript path has no parent directory")?
            .to_path_buf();
        // Ahead of claude: the project dir must exist to be watchable.
        std::fs::create_dir_all(&parent)
            .with_context(|| format!("creating {}", parent.display()))?;
        lock_unpoisoned(&self.filter)
            .registered
            .insert(tracked.transcript_path.clone());
        self.path_to_session
            .insert(tracked.transcript_path.clone(), session.to_owned());
        let count = self.watched_dirs.entry(parent.clone()).or_insert(0);
        *count += 1;
        if *count == 1
            && let Some(watcher) = self.watcher.as_mut()
            && let Err(e) =
                notify::Watcher::watch(watcher, &parent, notify::RecursiveMode::NonRecursive)
        {
            // Degraded, durable (the LOW-8 mold): stall detection for
            // this worker loses the transcript heartbeat; ledger events
            // still reset, and a false stall costs one nudge.
            ledger.append(EventInput {
                kind: EventType::PatrolDegraded,
                rig: None,
                actor: "campd".into(),
                bead: None,
                data: serde_json::json!({
                    "error": format!("transcript watch failed for {}: {e}", parent.display()),
                    "session": session,
                }),
            })?;
        }
        tracked.watch_registered = true;
        Ok(())
    }

    fn unwatch_transcript(&mut self, tracked: &Tracked) {
        lock_unpoisoned(&self.filter)
            .registered
            .remove(&tracked.transcript_path);
        self.path_to_session.remove(&tracked.transcript_path);
        let Some(parent) = tracked.transcript_path.parent() else {
            return;
        };
        if let Some(count) = self.watched_dirs.get_mut(parent) {
            *count -= 1;
            if *count == 0 {
                self.watched_dirs.remove(parent);
                if let Some(watcher) = self.watcher.as_mut() {
                    // Unwatch failures are non-events: the dir may be
                    // gone; the filter no longer matches its paths.
                    let _ = notify::Watcher::unwatch(watcher, parent);
                }
            }
        }
    }

    /// Consume watch-observed transcript activity: reset the touched
    /// sessions' timers (and rewind their ladders to nudge).
    pub fn drain_touched(&mut self, now: Timestamp) {
        let touched: Vec<PathBuf> = lock_unpoisoned(&self.filter).touched.drain().collect();
        for path in touched {
            if let Some(session) = self.path_to_session.get(&path) {
                let session = session.clone();
                if let Some(t) = self.tracked.get(&session) {
                    self.ladder.on_activity(&t.bead.clone());
                }
                self.timers.reset(&session, now);
            }
        }
    }

    /// Drain a stored watcher error into its durable event (the orders
    /// LOW-8 mold — never just stderr on a detached daemon).
    pub fn take_watch_error_events(&mut self) -> Vec<EventInput> {
        let Some(msg) = lock_unpoisoned(&self.filter).error.take() else {
            return Vec::new();
        };
        vec![EventInput {
            kind: EventType::PatrolDegraded,
            rig: None,
            actor: "campd".into(),
            bead: None,
            data: serde_json::json!({
                "error": format!("transcript watcher error: {msg}"),
            }),
        }]
    }

    /// Declare due stall fires durably — one agent.stalled each, action
    /// chosen by the ladder — and queue the actions for execute_pending.
    /// The declaration precedes the action (the declare_cron_fires mold).
    /// Returns whether anything was appended (drives wake_ledger_work).
    pub fn declare_stalls(&mut self, ledger: &mut Ledger, fires: &[StallFire]) -> Result<bool> {
        let mut declared = false;
        for fire in fires {
            match fire.kind {
                TimerKind::Release => {
                    // The grace expired; the kill is the action, the reap's
                    // reasoned session.stopped is the record.
                    self.pending.push(PendingAction::KillReleased {
                        session: fire.session.clone(),
                    });
                    continue;
                }
                TimerKind::Stall => {}
            }
            let Some(tracked) = self.tracked.get(&fire.session) else {
                continue; // untracked since the fire was computed
            };
            let tracked = tracked.clone();
            let action = if tracked.owned == Owned::Annotate {
                "annotate"
            } else {
                match self.ladder.on_fire(&tracked.bead) {
                    LadderAction::Nudge => "nudge",
                    LadderAction::Restart => "restart",
                    LadderAction::Exhausted => "exhausted",
                }
            };
            let seq = ledger.append(EventInput {
                kind: EventType::AgentStalled,
                rig: tracked.rig.clone(),
                actor: "campd".into(),
                bead: Some(tracked.bead.clone()),
                data: serde_json::json!({
                    "session": fire.session,
                    "agent": tracked.agent,
                    "action": action,
                    "threshold": threshold_string(fire.threshold),
                    "restarts": self.ladder.restarts(&tracked.bead),
                }),
            })?;
            declared = true;
            match action {
                "exhausted" => {
                    // Emit and STOP (spec §10): disarm, forget tracking;
                    // escalation is pack content matching agent.stalled.
                    self.timers.disarm(&fire.session);
                    if let Some(t) = self.tracked.remove(&fire.session) {
                        self.unwatch_transcript(&t);
                    }
                }
                "nudge" => {
                    self.pending.push(PendingAction::Nudge {
                        session: fire.session.clone(),
                        cause_seq: seq,
                    });
                    self.rearm(&fire.session, &tracked, fire.deadline);
                }
                "restart" => {
                    self.pending.push(PendingAction::Restart {
                        session: fire.session.clone(),
                        cause_seq: seq,
                    });
                    // Re-armed at the (now doubled) effective threshold: a
                    // successful kill untracks via the crash observation; a
                    // failed non-child kill retries at the next fire.
                    self.rearm(&fire.session, &tracked, fire.deadline);
                }
                _ => {
                    // annotate: re-arm, nothing mechanical beyond the event
                    self.rearm(&fire.session, &tracked, fire.deadline);
                }
            }
        }
        Ok(declared)
    }

    /// Re-arm anchored at the fired deadline (explicit-time discipline,
    /// plan Decision A): the next silence window starts where the last
    /// one demonstrably ended, deterministic under test.
    fn rearm(&mut self, session: &str, tracked: &Tracked, at: Timestamp) {
        let base = tracked.base_threshold.unwrap_or(self.config.stall_after);
        let effective = self.ladder.effective_threshold(&tracked.bead, base);
        self.timers.arm(session, TimerKind::Stall, effective, at);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use camp_core::clock::FixedClock;
    use camp_core::config::CampConfig;
    use camp_core::event::{Event, EventInput, EventType};
    use camp_core::ledger::Ledger;
    use jiff::Timestamp;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    /// A camp root with a ledger, one rig, and a `dev` agent definition.
    fn fixture() -> (tempfile::TempDir, Ledger, CampConfig, PatrolRuntime) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("camp.toml"),
            "[camp]\nname = \"t\"\n\n[[rigs]]\nname = \"gc\"\npath = \"/tmp\"\nprefix = \"gc\"\n",
        )
        .unwrap();
        let agents = dir.path().join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(agents.join("dev.md"), "---\nname: dev\n---\nWork.\n").unwrap();
        let ledger = Ledger::open_with_clock(
            &dir.path().join("camp.db"),
            Box::new(FixedClock::new("2026-07-07T07:00:00Z")),
        )
        .unwrap();
        let config = CampConfig::load(&dir.path().join("camp.toml")).unwrap();
        let patrol_config = camp_core::patrol::PatrolConfig::from_section(&config.patrol).unwrap();
        let patrol = PatrolRuntime::new(patrol_config, &config);
        (dir, ledger, config, patrol)
    }

    fn woke_event(
        ledger: &mut Ledger,
        name: &str,
        agent: &str,
        bead: &str,
        transcript: &std::path::Path,
        actor: &str,
    ) -> Event {
        seeded_bead(ledger, bead);
        let seq = ledger
            .append(EventInput {
                kind: EventType::SessionWoke,
                rig: Some("gc".into()),
                actor: actor.into(),
                bead: Some(bead.into()),
                data: serde_json::json!({
                    "name": name, "agent": agent, "rig": "gc",
                    "claude_session_id": "11111111-2222-4333-8444-555555555555",
                    "transcript_path": transcript,
                    "bead": bead,
                }),
            })
            .unwrap();
        ledger.events_range(seq, Some(seq)).unwrap().remove(0)
    }

    fn seeded_bead(ledger: &mut Ledger, id: &str) {
        if ledger.get_bead(id).unwrap().is_some() {
            return;
        }
        ledger
            .append(EventInput {
                kind: EventType::BeadCreated,
                rig: Some("gc".into()),
                actor: "test".into(),
                bead: Some(id.into()),
                data: serde_json::json!({"title": "t"}),
            })
            .unwrap();
    }

    fn stalled_events(ledger: &Ledger) -> Vec<Event> {
        ledger.events_of_type(EventType::AgentStalled).unwrap()
    }

    #[test]
    fn observe_woke_then_apply_arms_a_timer_and_registers_the_watch() {
        let (dir, mut ledger, _config, mut patrol) = fixture();
        let transcript = dir.path().join("projects/-p/sid.jsonl");
        let event = woke_event(&mut ledger, "t/dev/1", "dev", "gc-1", &transcript, "campd");
        let now = ts("2026-07-07T07:00:00Z");
        patrol.observe(&event);
        assert_eq!(
            patrol.poll_timeout(now),
            None,
            "observe is memory-only; arming happens at apply_tracking"
        );
        patrol.apply_tracking(&mut ledger, now).unwrap();
        assert!(patrol.poll_timeout(now).is_some(), "the timer is armed");
        assert!(
            transcript.parent().unwrap().is_dir(),
            "the watch dir is created ahead of claude"
        );
        assert!(
            patrol
                .filter_slot()
                .lock()
                .unwrap()
                .registered
                .contains(&transcript),
            "the transcript path is registered for the callback filter"
        );
        // default threshold: fires at stall_after (10m), not before
        assert!(patrol.fire_due(ts("2026-07-07T07:09:59Z")).is_empty());
        assert_eq!(patrol.fire_due(ts("2026-07-07T07:10:00Z")).len(), 1);
    }

    #[test]
    fn frontmatter_stall_after_governs_the_armed_threshold() {
        let (dir, mut ledger, _config, mut patrol) = fixture();
        // round-1 review note: the 5m override must actually arm at 5m
        std::fs::write(
            dir.path().join("agents/quick.md"),
            "---\nname: quick\nstall_after: 5m\n---\nWork fast.\n",
        )
        .unwrap();
        let transcript = dir.path().join("projects/-p/sid.jsonl");
        let event = woke_event(
            &mut ledger,
            "t/quick/1",
            "quick",
            "gc-1",
            &transcript,
            "campd",
        );
        let now = ts("2026-07-07T07:00:00Z");
        patrol.observe(&event);
        patrol.apply_tracking(&mut ledger, now).unwrap();
        assert!(patrol.fire_due(ts("2026-07-07T07:04:59Z")).is_empty());
        let fires = patrol.fire_due(ts("2026-07-07T07:05:00Z"));
        assert_eq!(fires.len(), 1, "the agent override (5m) armed the timer");
    }

    #[test]
    fn ledger_activity_resets_the_timer_by_all_three_keys() {
        let (dir, mut ledger, _config, mut patrol) = fixture();
        let transcript = dir.path().join("projects/-p/sid.jsonl");
        let event = woke_event(&mut ledger, "t/dev/1", "dev", "gc-1", &transcript, "campd");
        let t0 = ts("2026-07-07T07:00:00Z");
        patrol.observe(&event);
        patrol.apply_tracking(&mut ledger, t0).unwrap();

        // Decision J's three keys, each observed at 07:05 → deadline 07:15
        let activity: [(EventType, Option<&str>, &str, serde_json::Value); 3] = [
            // (a) bead match (worker.milestone --bead)
            (
                EventType::WorkerMilestone,
                Some("gc-1"),
                "cli",
                serde_json::json!({"text": "progress"}),
            ),
            // (b) actor == session name (event emit --session)
            (
                EventType::WorkerMilestone,
                None,
                "t/dev/1",
                serde_json::json!({"text": "note"}),
            ),
            // (c) data.session == session name (bead.claimed)
            (
                EventType::BeadClaimed,
                Some("gc-1"),
                "cli",
                serde_json::json!({"session": "t/dev/1"}),
            ),
        ];
        for (i, (kind, bead, actor, data)) in activity.into_iter().enumerate() {
            // fresh arm at t0 each round
            patrol.observe(&woke_or_reset_probe(
                &mut ledger,
                kind,
                bead,
                actor,
                data,
                i,
            ));
            patrol
                .apply_tracking(&mut ledger, ts("2026-07-07T07:05:00Z"))
                .unwrap();
            assert!(
                patrol.fire_due(ts("2026-07-07T07:10:00Z")).is_empty(),
                "key {i}: the old deadline must be gone"
            );
            assert_eq!(
                patrol.fire_due(ts("2026-07-07T07:15:00Z")).len(),
                1,
                "key {i}: the pushed deadline fires"
            );
            // re-arm for the next round
            let event = ledger
                .events_of_type(EventType::SessionWoke)
                .unwrap()
                .remove(0);
            patrol.observe(&event);
            patrol.apply_tracking(&mut ledger, t0).unwrap();
        }
    }

    /// Build a ledger event of the given shape for reset probing. The
    /// bead.claimed arm claims and immediately synthesizes; milestones
    /// append plainly.
    fn woke_or_reset_probe(
        ledger: &mut Ledger,
        kind: EventType,
        bead: Option<&str>,
        actor: &str,
        data: serde_json::Value,
        round: usize,
    ) -> Event {
        if kind == EventType::BeadClaimed {
            // a claimable bead per round (a bead claims once)
            let id = format!("gc-{}", 100 + round);
            seeded_bead(ledger, &id);
            let seq = ledger
                .append(EventInput {
                    kind,
                    rig: Some("gc".into()),
                    actor: actor.into(),
                    bead: Some(id),
                    data,
                })
                .unwrap();
            return ledger.events_range(seq, Some(seq)).unwrap().remove(0);
        }
        let seq = ledger
            .append(EventInput {
                kind,
                rig: Some("gc".into()),
                actor: actor.into(),
                bead: bead.map(str::to_owned),
                data,
            })
            .unwrap();
        ledger.events_range(seq, Some(seq)).unwrap().remove(0)
    }

    #[test]
    fn transcript_touch_resets_via_the_filter() {
        let (dir, mut ledger, _config, mut patrol) = fixture();
        let transcript = dir.path().join("projects/-p/sid.jsonl");
        let event = woke_event(&mut ledger, "t/dev/1", "dev", "gc-1", &transcript, "campd");
        patrol.observe(&event);
        patrol
            .apply_tracking(&mut ledger, ts("2026-07-07T07:00:00Z"))
            .unwrap();

        // the notify callback observed activity on the registered path...
        let mut hit = notify::Event::new(notify::EventKind::Any);
        hit.paths.push(transcript.clone());
        on_watch_event(Ok(hit), None, &patrol.filter_slot());
        // ...and an unrelated path, which must not reset anything
        let mut other = notify::Event::new(notify::EventKind::Any);
        other.paths.push(dir.path().join("projects/-p/other.jsonl"));
        on_watch_event(Ok(other), None, &patrol.filter_slot());

        patrol.drain_touched(ts("2026-07-07T07:09:00Z"));
        assert!(
            patrol.fire_due(ts("2026-07-07T07:10:00Z")).is_empty(),
            "the touch pushed the deadline"
        );
        assert_eq!(patrol.fire_due(ts("2026-07-07T07:19:00Z")).len(), 1);
    }

    #[test]
    fn watch_errors_become_durable_patrol_degraded() {
        let (_dir, _ledger, _config, patrol) = fixture();
        let slot = patrol.filter_slot();
        on_watch_event(
            Err(notify::Error::generic("inotify watch limit reached")),
            None,
            &slot,
        );
        let mut patrol = patrol;
        let events = patrol.take_watch_error_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, EventType::PatrolDegraded);
        assert!(
            events[0].data["error"]
                .as_str()
                .unwrap()
                .contains("inotify watch limit reached")
        );
        assert!(patrol.take_watch_error_events().is_empty(), "drained");
    }

    #[test]
    fn declare_stalls_appends_agent_stalled_with_the_ladder_action_and_cause() {
        let (dir, mut ledger, _config, mut patrol) = fixture();
        let transcript = dir.path().join("projects/-p/sid.jsonl");
        let event = woke_event(&mut ledger, "t/dev/1", "dev", "gc-1", &transcript, "campd");
        patrol.observe(&event);
        patrol
            .apply_tracking(&mut ledger, ts("2026-07-07T07:00:00Z"))
            .unwrap();

        // first fire: nudge
        let fires = patrol.fire_due(ts("2026-07-07T07:10:00Z"));
        assert!(patrol.declare_stalls(&mut ledger, &fires).unwrap());
        let stalled = stalled_events(&ledger);
        assert_eq!(stalled.len(), 1);
        assert_eq!(stalled[0].data["action"], "nudge");
        assert_eq!(stalled[0].data["session"], "t/dev/1");
        assert_eq!(stalled[0].data["agent"], "dev");
        assert_eq!(stalled[0].data["threshold"], "600s");
        assert_eq!(stalled[0].data["restarts"], 0);
        assert_eq!(stalled[0].bead.as_deref(), Some("gc-1"));
        assert_eq!(stalled[0].actor, "campd");

        // still silent: the re-armed timer fires again → restart
        let fires = patrol.fire_due(ts("2026-07-07T07:20:00Z"));
        assert_eq!(fires.len(), 1, "the nudge declaration re-armed the timer");
        patrol.declare_stalls(&mut ledger, &fires).unwrap();
        let stalled = stalled_events(&ledger);
        assert_eq!(stalled.len(), 2);
        assert_eq!(stalled[1].data["action"], "restart");
        assert_eq!(stalled[1].data["restarts"], 1);
    }

    /// ROUND-1 BLOCKER 1 REGRESSION PIN: patrol's own agent.stalled (actor
    /// campd, carrying the worker's bead and data.session) must NOT read
    /// as worker activity — otherwise the ladder rewinds to Nudge on the
    /// settle after every declaration and Restart is unreachable.
    #[test]
    fn patrols_own_events_do_not_rewind_the_ladder() {
        let (dir, mut ledger, _config, mut patrol) = fixture();
        let transcript = dir.path().join("projects/-p/sid.jsonl");
        let event = woke_event(&mut ledger, "t/dev/1", "dev", "gc-1", &transcript, "campd");
        patrol.observe(&event);
        patrol
            .apply_tracking(&mut ledger, ts("2026-07-07T07:00:00Z"))
            .unwrap();

        let fires = patrol.fire_due(ts("2026-07-07T07:10:00Z"));
        patrol.declare_stalls(&mut ledger, &fires).unwrap();
        // the settle's catch-up now observes the just-appended declaration
        let declared = stalled_events(&ledger).remove(0);
        patrol.observe(&declared);
        patrol
            .apply_tracking(&mut ledger, ts("2026-07-07T07:10:00Z"))
            .unwrap();
        // ...and observing a campd session.crashed untracks WITHOUT
        // on_activity (exclusive dispatch): covered below by escalation.
        let fires = patrol.fire_due(ts("2026-07-07T07:20:00Z"));
        assert_eq!(fires.len(), 1);
        patrol.declare_stalls(&mut ledger, &fires).unwrap();
        let stalled = stalled_events(&ledger);
        assert_eq!(
            stalled[1].data["action"], "restart",
            "the ladder must ESCALATE despite observing its own declaration"
        );
    }

    #[test]
    fn session_end_untracks_and_exhaustion_stops() {
        let (dir, mut ledger, config, _patrol) = fixture();
        // budget 0: nudge then exhausted (emit-and-stop)
        let patrol_config = camp_core::patrol::PatrolConfig {
            stall_after: jiff::SignedDuration::from_mins(10),
            restart_budget: 0,
            release_grace: jiff::SignedDuration::from_secs(30),
        };
        let mut patrol = PatrolRuntime::new(patrol_config, &config);
        let transcript = dir.path().join("projects/-p/sid.jsonl");
        let event = woke_event(&mut ledger, "t/dev/1", "dev", "gc-1", &transcript, "campd");
        patrol.observe(&event);
        patrol
            .apply_tracking(&mut ledger, ts("2026-07-07T07:00:00Z"))
            .unwrap();

        // stopped → untracked, disarmed
        let seq = ledger
            .append(EventInput {
                kind: EventType::SessionStopped,
                rig: Some("gc".into()),
                actor: "campd".into(),
                bead: None,
                data: serde_json::json!({"name": "t/dev/1", "exit_code": 0}),
            })
            .unwrap();
        let end = ledger.events_range(seq, Some(seq)).unwrap().remove(0);
        patrol.observe(&end);
        patrol
            .apply_tracking(&mut ledger, ts("2026-07-07T07:01:00Z"))
            .unwrap();
        assert_eq!(patrol.poll_timeout(ts("2026-07-07T07:01:00Z")), None);

        // re-track (a fresh woke row) and run the budget-0 ladder out
        let event2 = woke_event(&mut ledger, "t/dev/2", "dev", "gc-2", &transcript, "campd");
        patrol.observe(&event2);
        patrol
            .apply_tracking(&mut ledger, ts("2026-07-07T07:02:00Z"))
            .unwrap();
        let fires = patrol.fire_due(ts("2026-07-07T07:12:00Z"));
        patrol.declare_stalls(&mut ledger, &fires).unwrap(); // nudge
        let fires = patrol.fire_due(ts("2026-07-07T07:22:00Z"));
        patrol.declare_stalls(&mut ledger, &fires).unwrap(); // exhausted
        let stalled = stalled_events(&ledger);
        assert_eq!(stalled.last().unwrap().data["action"], "exhausted");
        assert_eq!(
            patrol.poll_timeout(ts("2026-07-07T07:22:00Z")),
            None,
            "exhaustion disarms: emit and STOP"
        );
    }

    #[test]
    fn annotate_owned_sessions_never_escalate() {
        let (dir, mut ledger, _config, mut patrol) = fixture();
        let transcript = dir.path().join("projects/-p/att.jsonl");
        // an attended registration: actor is a hook, not campd
        let event = woke_event(
            &mut ledger,
            "att/1",
            "dev",
            "gc-9",
            &transcript,
            "hook:session-start",
        );
        patrol.observe(&event);
        patrol
            .apply_tracking(&mut ledger, ts("2026-07-07T07:00:00Z"))
            .unwrap();
        for (i, at) in ["2026-07-07T07:10:00Z", "2026-07-07T07:20:00Z"]
            .iter()
            .enumerate()
        {
            let fires = patrol.fire_due(ts(at));
            assert_eq!(fires.len(), 1, "round {i}: annotate re-arms");
            patrol.declare_stalls(&mut ledger, &fires).unwrap();
        }
        for e in stalled_events(&ledger) {
            assert_eq!(
                e.data["action"], "annotate",
                "attended sessions annotate only, never nudge/restart"
            );
        }
    }
}

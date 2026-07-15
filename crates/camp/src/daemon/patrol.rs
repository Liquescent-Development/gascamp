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
//! Patrol config is swapped on an applied hot reload (issue #81,
//! `apply_config`): future agent/rig/threshold resolutions and future
//! timer arms follow the reloaded config with no campd restart. In-flight
//! stall timers are NOT re-armed — each tracked worker keeps the threshold
//! it was armed with (the surviving half of Phase-11 plan Decision L; the
//! config-visibility half is un-deferred by #81).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, Result};
use camp_core::Seq;

use super::dispatch::{Dispatcher, NudgeOutcome};
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
    /// The CANONICAL transcript path the watch filter is keyed on: the
    /// watch backend (FSEvents/inotify) reports canonicalized paths, and
    /// tempdirs/symlinked homes would otherwise never match (macOS /var →
    /// /private/var). Set at apply_tracking; used to unwatch.
    watch_key: Option<PathBuf>,
    /// F7 pins as recorded on the woke event (Phase 3, #48 finding 1) —
    /// re-applied on the nudge-resume path; None = a bare resume.
    model: Option<String>,
    permission_mode: Option<String>,
    allowed_tools: Option<String>,
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
    // boxed: Tracked is ~230 bytes and Untrack is a bare String
    // (clippy::large_enum_variant)
    Track {
        session: String,
        tracked: Box<Tracked>,
    },
    Untrack {
        session: String,
    },
}

/// Ladder actions queued by `declare_stalls` (after the durable
/// agent.stalled append) and release work queued by `observe`; executed
/// by `execute_pending` (Task 11.11).
#[derive(Debug, PartialEq)]
pub(super) enum PendingAction {
    Nudge {
        session: String,
        cause_seq: Seq,
    },
    Restart {
        session: String,
        cause_seq: Seq,
    },
    /// Re-hook the bead after a patrol-caused crash: a TARGETED dispatch
    /// (Dispatcher::dispatch_bead) — the general dispatchable set
    /// deliberately excludes ever-sessioned beads (Phase 8 decision C) so
    /// organic crashes cannot hot-loop; the ladder budget bounds these.
    Respawn {
        bead: String,
    },
    Release {
        bead: String,
    },
    KillReleased {
        session: String,
    },
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
    /// Sessions currently in a declared-stalled state — SET when a stall
    /// is declared (nudge/restart/annotate; NOT exhausted, which untracks)
    /// and CLEARED at every timer-reset / untrack site. Backs the status
    /// socket's `red` count (Phase 12 Decision D2). There is no durable
    /// "un-stalled" event, so this in-memory set — not the timer store,
    /// which `fire_due` empties and `declare_stalls` re-arms — is the
    /// source of truth for "currently stalled".
    stalled: HashSet<String>,
    /// cp-3 (§5.3.3): sessions patrol has seen BLOCKED (a pending permission).
    /// A blocked worker is not a stalled worker — it is WAITING ON US, so its
    /// stall timer is DISARMED (it adds no wakeup, invariant 1) and it is exempt
    /// from the ladder. Reconciled edge-triggered from ledger truth each wake by
    /// `reconcile_blocked`; the disarm happens on working→blocked, the re-arm on
    /// blocked→working (a decision).
    blocked: HashSet<String>,
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
            stalled: HashSet::new(),
            blocked: HashSet::new(),
        }
    }

    /// The number of currently-stalled tracked sessions — the `✖red` of
    /// the fleet badge (Decision D2). Intersected with `tracked` so a
    /// missed clear can never inflate the count.
    pub fn stalled_count(&self) -> u64 {
        self.stalled
            .iter()
            .filter(|s| self.tracked.contains_key(*s))
            .count() as u64
    }

    /// cp-1 (§4.1): is THIS session stalled?
    ///
    /// It uses the SAME `tracked` intersection `stalled_count` applies (*"a
    /// missed clear can never inflate the count"*). Divergent semantics between
    /// the fleet count and the per-session answer would be a bug, not a shortcut:
    /// an operator who sees `red: 1` in `status` and no stalled session in
    /// `sessions.list` has been told two different stories by one daemon.
    pub fn is_stalled(&self, session: &str) -> bool {
        self.stalled.contains(session) && self.tracked.contains_key(session)
    }

    /// Swap patrol's config on an applied hot reload (issue #81). Patrol
    /// resolves agents, rig lookups, the dispatch command, and stall/
    /// release thresholds against the config it holds; an applied reload
    /// that adds a pack/agent/rig or edits `[patrol]` must reach patrol too,
    /// or a worker dispatched to a freshly added pack agent draws a spurious
    /// `patrol.degraded` "unknown agent" (the birth config cannot see it).
    ///
    /// FUTURE resolutions and future timer arms see the new config; in-flight
    /// timers and tracked workers are NOT re-armed — each armed worker keeps
    /// the threshold it was armed with, exactly as the dispatcher leaves
    /// in-flight children on their already-resolved spec. The ladder's
    /// per-bead restart history is preserved; only its `restart_budget`
    /// ceiling follows the reload.
    ///
    /// An applied config is pre-validated (`CampConfig::parse` runs
    /// `PatrolConfig::from_section`), so the re-derivation cannot fail for an
    /// applied reload; the `?` is fail-fast on an impossible torn state,
    /// never a silent fallback.
    pub fn apply_config(&mut self, config: CampConfig) -> Result<()> {
        let patrol_config = PatrolConfig::from_section(&config.patrol)?;
        self.ladder.set_restart_budget(patrol_config.restart_budget);
        self.config = patrol_config;
        self.camp_config = config;
        Ok(())
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

    /// cp-3 (§5.3.3): bring patrol's timers in line with the ledger's BLOCKED
    /// set. On the working→blocked edge, DISARM (a waiting worker is not a
    /// stalled worker — it must add no wakeup and take no ladder action). On
    /// blocked→working (a decision), RE-ARM from `now` (the worker is presumed
    /// working again). Idempotent, so it is safe to call from both the
    /// pre-ladder drain seam and the common post-harvest path each wake.
    pub fn reconcile_blocked(&mut self, ledger: &Ledger, now: Timestamp) -> Result<()> {
        let ledger_blocked: HashSet<String> = ledger.blocked_sessions()?.into_iter().collect();
        // working → blocked: disarm and record.
        for s in ledger_blocked
            .difference(&self.blocked)
            .cloned()
            .collect::<Vec<_>>()
        {
            self.timers.disarm(&s);
            self.blocked.insert(s);
        }
        // blocked → working: a decision arrived — re-arm from zero (only a
        // tracked worker has a threshold to re-arm; an adopted, untracked
        // worker's silence is owned by the adoption kill, not the ladder).
        for s in self
            .blocked
            .difference(&ledger_blocked)
            .cloned()
            .collect::<Vec<_>>()
        {
            if let Some(t) = self.tracked.get(&s).cloned() {
                self.rearm(&s, &t, now);
            }
            self.blocked.remove(&s);
        }
        Ok(())
    }

    /// cp-3 test accessor: is `session`'s stall timer currently armed? Reads the
    /// timer store directly — the invariant-1 disarm guard and the re-arm guard
    /// assert on it. `#[cfg(test)]` and `pub` so the external daemon_patrol
    /// tests and the inline unit guards both reach it.
    #[cfg(test)]
    pub fn is_armed(&self, session: &str) -> bool {
        self.timers.is_armed(session)
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
                    // A crash PATROL caused re-hooks the bead (spec §10.2
                    // "restart (kill, respawn, re-hook the bead)"): queue
                    // the targeted respawn before the row untracks.
                    if event.kind == EventType::SessionCrashed
                        && event.data["reason"]
                            .as_str()
                            .is_some_and(|r| r.starts_with("patrol restart"))
                        && let Some(t) = self.tracked.get(name)
                    {
                        self.pending.push(PendingAction::Respawn {
                            bead: t.bead.clone(),
                        });
                    }
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
            // compat §6.2 — drain-ack is the PROMPT KILL trigger. bead-close
            // only drops stdin + arms the grace (Release, above); drain-ack now
            // reaps the already-released worker IMMEDIATELY via kill_released,
            // instead of waiting the full release_grace. Idempotent: the grace
            // timer stays armed as the backstop for a worker that never acks,
            // and its later fire finds the already-reaped worker and no-ops
            // (kill_released's `released.is_some()` guard). If the ack somehow
            // precedes the release, kill_released no-ops too — safe either way.
            EventType::WorkerDrainAcked => {
                if let Some(session) = event.data["session"].as_str() {
                    self.pending.push(PendingAction::KillReleased {
                        session: session.to_owned(),
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
            tracked: Box::new(Tracked {
                bead: bead.to_owned(),
                agent: agent.to_owned(),
                rig: data["rig"].as_str().map(str::to_owned),
                claude_session_id: data["claude_session_id"].as_str().map(str::to_owned),
                transcript_path: PathBuf::from(transcript),
                worktree: data["worktree"].as_str().map(PathBuf::from),
                owned,
                base_threshold: None,
                watch_key: None,
                model: data["model"].as_str().map(str::to_owned),
                permission_mode: data["permission_mode"].as_str().map(str::to_owned),
                allowed_tools: data["allowed_tools"].as_str().map(str::to_owned),
            }),
        });
    }

    /// Re-arm a tracked session for adoption (Decision F): the caller
    /// verified the process alive at `pid`; the timer starts fresh
    /// (restart grace) and later restarts go through the probe-first
    /// non-child path.
    /// Whether patrol already tracks this session (adopt skips these —
    /// round-1 minor 4).
    pub fn is_tracked(&self, session: &str) -> bool {
        self.tracked.contains_key(session)
    }

    /// Adopt a live registry row (Decision F): track at the probed pid,
    /// annotate-only when the row was not campd-spawned.
    fn adopt_from_row(&mut self, row: &camp_core::ledger::SessionRow, pid: i64) {
        let (Some(transcript), Some(bead)) = (row.transcript_path.as_deref(), row.bead.as_deref())
        else {
            return; // callers check; belt-and-braces
        };
        let owned = if row.woke_actor == "campd" {
            Owned::AdoptedPid(pid)
        } else {
            Owned::Annotate
        };
        self.track_ops.push(TrackOp::Track {
            session: row.name.clone(),
            tracked: Box::new(Tracked {
                bead: bead.to_owned(),
                agent: row.agent.clone(),
                rig: row.rig.clone(),
                claude_session_id: row.claude_session_id.clone(),
                transcript_path: PathBuf::from(transcript),
                worktree: row.worktree.as_deref().map(PathBuf::from),
                owned,
                base_threshold: None,
                watch_key: None,
                model: row.model.clone(),
                permission_mode: row.permission_mode.clone(),
                allowed_tools: row.allowed_tools.clone(),
            }),
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
                    // HIGH 1 (fix-2 guard): a Track for an ALREADY-tracked
                    // session is a stale replay by construction — session
                    // names are fold-unique, so a second woke for the same
                    // name is the SAME row re-observed (e.g. a woke row
                    // past the cursor replayed by the startup settle).
                    // Skipping it keeps the first arming (and its Owned
                    // classification) authoritative — a re-Track would
                    // overwrite and re-arm from a duplicate.
                    if self.tracked.contains_key(&session) {
                        continue;
                    }
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
                    self.stalled.remove(&session); // fresh arm: never stalled (name-reuse guard)
                    self.tracked.insert(session, *tracked);
                }
                TrackOp::Untrack { session } => {
                    self.timers.disarm(&session);
                    if let Some(tracked) = self.tracked.remove(&session) {
                        self.unwatch_transcript(&tracked);
                    }
                    self.stalled.remove(&session); // ended/closed/crashed → not stalled
                }
            }
        }
        for session in std::mem::take(&mut self.activity) {
            self.timers.reset(&session, now);
            self.stalled.remove(&session); // worker activity revives it
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
        // The watch backend reports CANONICAL paths (macOS /var →
        // /private/var, symlinked homes): key the filter on them or the
        // touches never match.
        let parent = parent
            .canonicalize()
            .with_context(|| format!("canonicalizing {}", parent.display()))?;
        let file_name = tracked
            .transcript_path
            .file_name()
            .context("transcript path has no file name")?;
        let watch_key = parent.join(file_name);
        lock_unpoisoned(&self.filter)
            .registered
            .insert(watch_key.clone());
        self.path_to_session
            .insert(watch_key.clone(), session.to_owned());
        tracked.watch_key = Some(watch_key);
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
        Ok(())
    }

    fn unwatch_transcript(&mut self, tracked: &Tracked) {
        let Some(watch_key) = tracked.watch_key.as_ref() else {
            return; // never registered (apply_tracking not reached)
        };
        lock_unpoisoned(&self.filter).registered.remove(watch_key);
        self.path_to_session.remove(watch_key);
        let Some(parent) = watch_key.parent() else {
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
                self.stalled.remove(&session); // transcript heartbeat revives it
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
    /// `now` is the wake's instant: re-arms anchor at max(deadline, now)
    /// so a lagging wake still grants full revival grace after a nudge.
    /// Returns whether anything was appended (drives wake_ledger_work).
    pub fn declare_stalls(
        &mut self,
        ledger: &mut Ledger,
        fires: &[StallFire],
        now: Timestamp,
    ) -> Result<bool> {
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
            // §5.3.3: a BLOCKED session is exempt from the ENTIRE ladder — no
            // agent.stalled, no on_fire (which would burn a restart-budget
            // increment), no action. Belt-and-braces with reconcile_blocked's
            // disarm: a fire that slips through (the timer popped before the
            // reconcile disarmed it) is swallowed here and the timer left
            // disarmed. THE HEART PROPERTY: no BLOCKED worker is ever nudged,
            // restarted, or killed by the ladder.
            if self.blocked.contains(&fire.session) {
                self.timers.disarm(&fire.session);
                continue;
            }
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
                    self.stalled.remove(&fire.session); // untracked → drop the red flag
                }
                "nudge" => {
                    self.pending.push(PendingAction::Nudge {
                        session: fire.session.clone(),
                        cause_seq: seq,
                    });
                    self.stalled.insert(fire.session.clone());
                    self.rearm(&fire.session, &tracked, fire.deadline.max(now));
                }
                "restart" => {
                    self.pending.push(PendingAction::Restart {
                        session: fire.session.clone(),
                        cause_seq: seq,
                    });
                    self.stalled.insert(fire.session.clone());
                    // Re-armed at the (now doubled) effective threshold: a
                    // successful kill untracks via the crash observation; a
                    // failed non-child kill retries at the next fire.
                    self.rearm(&fire.session, &tracked, fire.deadline.max(now));
                }
                _ => {
                    // annotate: re-arm, nothing mechanical beyond the event
                    self.stalled.insert(fire.session.clone());
                    self.rearm(&fire.session, &tracked, fire.deadline.max(now));
                }
            }
        }
        Ok(declared)
    }

    /// Re-arm anchored at max(fired deadline, wake now) — explicit-time
    /// discipline (plan Decision A), and a lagging wake still grants a
    /// delivered nudge the full threshold of revival grace before any
    /// escalation.
    fn rearm(&mut self, session: &str, tracked: &Tracked, at: Timestamp) {
        let base = tracked.base_threshold.unwrap_or(self.config.stall_after);
        let effective = self.ladder.effective_threshold(&tracked.bead, base);
        self.timers.arm(session, TimerKind::Stall, effective, at);
    }

    /// Execute the queued ladder actions. Every action's declaration is
    /// already durable (declare_stalls); failures here append their own
    /// records (nudge_failed / patrol.degraded) — never silent, never
    /// fatal to campd (only ledger errors surface).
    pub fn execute_pending(
        &mut self,
        ledger: &mut Ledger,
        dispatcher: &mut Dispatcher,
        now: Timestamp,
    ) -> Result<()> {
        for action in std::mem::take(&mut self.pending) {
            match action {
                PendingAction::Nudge { session, cause_seq } => {
                    self.do_nudge(ledger, dispatcher, &session, cause_seq)?;
                }
                PendingAction::Restart { session, cause_seq } => {
                    self.do_restart(ledger, dispatcher, &session, cause_seq)?;
                }
                PendingAction::Respawn { bead } => {
                    dispatcher.dispatch_bead(ledger, &bead)?;
                }
                PendingAction::Release { bead } => {
                    if let Some(session) =
                        dispatcher.release_worker(&bead, "released after bead close")
                    {
                        // C2: the grace bounds the linger (P3: no exit on
                        // EOF). The Release timer replaces the stall timer.
                        self.timers.arm(
                            &session,
                            TimerKind::Release,
                            self.config.release_grace,
                            now,
                        );
                    } else if let Some(session) = self
                        .tracked
                        .iter()
                        .find(|(_, t)| t.bead == bead && t.owned != Owned::Annotate)
                        .map(|(name, _)| name.clone())
                    {
                        // HIGH 1: a non-child worker whose bead closed — an
                        // adopted worker OR one mislabeled Owned::Child
                        // after a campd-crash orphaning (release_worker
                        // already declined it, so this dispatcher does not
                        // hold it). NEVER an attended session (spec §10:
                        // t.owned != Annotate). Release the observation
                        // way: probe, kill, reasoned stop.
                        self.release_adopted(ledger, dispatcher, &session)?;
                    }
                }
                PendingAction::KillReleased { session } => {
                    dispatcher.kill_released(&session);
                    // the reap turns the exit into the reasoned stop
                }
            }
        }
        Ok(())
    }

    fn do_nudge(
        &mut self,
        ledger: &mut Ledger,
        dispatcher: &mut Dispatcher,
        session: &str,
        _cause_seq: Seq,
    ) -> Result<()> {
        let Some(tracked) = self.tracked.get(session).cloned() else {
            return Ok(()); // untracked since the declaration
        };
        let base = tracked.base_threshold.unwrap_or(self.config.stall_after);
        let text = nudge_text(
            &tracked.bead,
            session,
            &threshold_string(self.ladder.effective_threshold(&tracked.bead, base)),
        );
        if dispatcher.is_child(session) {
            match dispatcher.nudge_via_stdin(session, &text) {
                NudgeOutcome::Delivered => return Ok(()),
                NudgeOutcome::Failed(e) => {
                    return self.nudge_failed(ledger, session, &tracked, "stdin", &e);
                }
                NudgeOutcome::NoPipe => {} // released or Null-mode: resume
            }
        }
        // The resume path (spec §10 as amended: "otherwise via session
        // resume"; A4-4 two-writers caution documented). The nudge child
        // is an aux process: reaped by the dispatcher, failure evented as
        // patrol.degraded.
        let Some(sid) = tracked.claude_session_id.as_deref() else {
            return self.nudge_failed(
                ledger,
                session,
                &tracked,
                "resume",
                "the registry row has no claude session id to resume",
            );
        };
        let cwd = match &tracked.worktree {
            Some(wt) => wt.clone(),
            None => match tracked
                .rig
                .as_deref()
                .and_then(|r| self.camp_config.rig(r).ok())
            {
                Some(rig) => rig.path.clone(),
                None => {
                    return self.nudge_failed(
                        ledger,
                        session,
                        &tracked,
                        "resume",
                        "no worktree and no configured rig to run the resume in",
                    );
                }
            },
        };
        let log_path = self
            .camp_config
            .root
            .clone()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("sessions")
            .join(format!(
                "{}.nudge.log",
                crate::daemon::spawn::munge(session)
            ));
        let spawn_result = (|| -> Result<std::process::Command> {
            if let Some(parent) = log_path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            let log = std::fs::File::create(&log_path)
                .with_context(|| format!("creating {}", log_path.display()))?;
            let log_err = log.try_clone().context("cloning the nudge log handle")?;
            // One resume argv vocabulary (spawn::resume_argv): the F7 pins
            // recorded at spawn ride the resume too (#48 finding 1).
            let pins = crate::daemon::spawn::ResumePins {
                model: tracked.model.clone(),
                permission_mode: tracked.permission_mode.clone(),
                allowed_tools: tracked.allowed_tools.clone(),
            };
            let mut cmd = std::process::Command::new(&self.camp_config.dispatch.command);
            cmd.args(crate::daemon::spawn::resume_argv(sid, &text, &pins))
                .current_dir(&cwd)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::from(log))
                .stderr(std::process::Stdio::from(log_err));
            Ok(cmd)
        })();
        let outcome =
            spawn_result.and_then(|cmd| dispatcher.spawn_aux(session, "nudge-resume", cmd));
        if let Err(e) = outcome {
            return self.nudge_failed(ledger, session, &tracked, "resume", &format!("{e:#}"));
        }
        Ok(())
    }

    /// A nudge that could not be DELIVERED (plan Decision E): durable
    /// nudge_failed record, ladder advances to restart, timer re-arms.
    fn nudge_failed(
        &mut self,
        ledger: &mut Ledger,
        session: &str,
        tracked: &Tracked,
        via: &str,
        error: &str,
    ) -> Result<()> {
        let base = tracked.base_threshold.unwrap_or(self.config.stall_after);
        ledger.append(EventInput {
            kind: EventType::AgentStalled,
            rig: tracked.rig.clone(),
            actor: "campd".into(),
            bead: Some(tracked.bead.clone()),
            data: serde_json::json!({
                "session": session,
                "agent": tracked.agent,
                "action": "nudge_failed",
                "threshold": threshold_string(
                    self.ladder.effective_threshold(&tracked.bead, base)
                ),
                "restarts": self.ladder.restarts(&tracked.bead),
                "via": via,
                "error": error,
            }),
        })?;
        self.ladder.nudge_failed(&tracked.bead);
        Ok(())
    }

    fn do_restart(
        &mut self,
        ledger: &mut Ledger,
        dispatcher: &mut Dispatcher,
        session: &str,
        cause_seq: Seq,
    ) -> Result<()> {
        let Some(tracked) = self.tracked.get(session).cloned() else {
            return Ok(());
        };
        // Never restart an attended session (spec §10: never kill a
        // session in the user's TUI). declare never queues these; the
        // guard is belt-and-braces before any kill path.
        if tracked.owned == Owned::Annotate {
            return Ok(());
        }
        // HIGH 1: trust the LIVE dispatcher, not the Owned label. A worker
        // orphaned across a campd crash (its session.woke row past the
        // cursor) is re-tracked as Owned::Child by the startup settle
        // (actor=="campd") even though THIS dispatcher never held it —
        // `kill_worker` then returns false. The old code discarded that
        // bool: no kill, no session.crashed, no bead release, while a
        // false agent.stalled{restart} recorded a restart that never
        // happened (invariants 3/5). Only a kill_worker that RETURNS TRUE
        // (a genuine child of this campd) takes the SIGCHLD path; every
        // other case falls through to the probe-verified non-child kill.
        if tracked.owned == Owned::Child && dispatcher.kill_worker(session, cause_seq) {
            // SIGCHLD does the rest: caused crash, fold release, converge
            // respawn — each its own event.
            return Ok(());
        }
        self.restart_non_child(ledger, dispatcher, session, &tracked, cause_seq)
    }

    /// Kill a NON-child worker (adopted, or a genuine-looking Child this
    /// dispatcher does not hold) by probe-verified pid, recording the
    /// caused crash. ROUND-1 BLOCKER 2: re-probe by uuid IMMEDIATELY
    /// before the kill and kill the re-probed pid only — the pid observed
    /// earlier may be stale and REUSED by an innocent process (no SIGCHLD
    /// for non-children).
    fn restart_non_child(
        &mut self,
        ledger: &mut Ledger,
        dispatcher: &mut Dispatcher,
        session: &str,
        tracked: &Tracked,
        cause_seq: Seq,
    ) -> Result<()> {
        let exec_timeout = self.camp_config.dispatch.exec_timeout()?;
        let probed = probe_alive(
            tracked.claude_session_id.as_deref(),
            None,
            &dispatcher.known_pids(),
            &self.camp_config.dispatch.command,
            exec_timeout,
        )?;
        let Some(pid) = probed else {
            // already dead: record the caused crash, no kill
            ledger.append(EventInput {
                kind: EventType::SessionCrashed,
                rig: tracked.rig.clone(),
                actor: "campd".into(),
                bead: None,
                data: serde_json::json!({
                    "name": session,
                    "reason": "patrol restart: found dead at restart",
                    "cause_seq": cause_seq,
                }),
            })?;
            return Ok(());
        };
        kill_pid(pid, exec_timeout)?;
        // verify the death by observation before recording (LOW 4: never
        // by kill's exit chatter)
        if probe_alive(
            tracked.claude_session_id.as_deref(),
            None,
            &dispatcher.known_pids(),
            &self.camp_config.dispatch.command,
            exec_timeout,
        )?
        .is_some()
        {
            // Not confirmably dead: degraded, timer stays armed (declare
            // re-armed it), retry next fire.
            ledger.append(EventInput {
                kind: EventType::PatrolDegraded,
                rig: None,
                actor: "campd".into(),
                bead: None,
                data: serde_json::json!({
                    "error": format!(
                        "restart kill of pid {pid} did not take; retrying at the next fire"
                    ),
                    "session": session,
                }),
            })?;
            return Ok(());
        }
        ledger.append(EventInput {
            kind: EventType::SessionCrashed,
            rig: tracked.rig.clone(),
            actor: "campd".into(),
            bead: None,
            data: serde_json::json!({
                "name": session,
                "reason": "patrol restart",
                "cause_seq": cause_seq,
            }),
        })?;
        Ok(())
    }

    /// The adopt()/bead-closed release rule for NON-child workers: probe,
    /// kill the re-probed pid, record the reasoned stop, untrack.
    fn release_adopted(
        &mut self,
        ledger: &mut Ledger,
        dispatcher: &mut Dispatcher,
        session: &str,
    ) -> Result<()> {
        let Some(tracked) = self.tracked.get(session).cloned() else {
            return Ok(());
        };
        let exec_timeout = self.camp_config.dispatch.exec_timeout()?;
        if let Some(pid) = probe_alive(
            tracked.claude_session_id.as_deref(),
            None,
            &dispatcher.known_pids(),
            &self.camp_config.dispatch.command,
            exec_timeout,
        )? {
            kill_pid(pid, exec_timeout)?;
            // Classification by RE-PROBE (round-2 LOW 4): the stop record
            // rests on observed death, never on kill's exit chatter.
            if probe_alive(
                tracked.claude_session_id.as_deref(),
                None,
                &dispatcher.known_pids(),
                &self.camp_config.dispatch.command,
                exec_timeout,
            )?
            .is_some()
            {
                ledger.append(EventInput {
                    kind: EventType::PatrolDegraded,
                    rig: None,
                    actor: "campd".into(),
                    bead: None,
                    data: serde_json::json!({
                        "error": format!(
                            "release kill of pid {pid} did not take; the session stays live"
                        ),
                        "session": session,
                    }),
                })?;
                return Ok(());
            }
        }
        ledger.append(EventInput {
            kind: EventType::SessionStopped,
            rig: tracked.rig.clone(),
            actor: "campd".into(),
            bead: None,
            data: serde_json::json!({
                "name": session,
                "reason": "released after bead close",
            }),
        })?;
        // the observation of that stop untracks on the next catch-up; the
        // timer goes now so no fire lands in between
        self.timers.disarm(session);
        Ok(())
    }
}

/// One `camp adopt` outcome (spec §8.5): what reconciliation observed and
/// did. All-zero on a second run — adoption is idempotent (already-tracked
/// sessions are skipped; dispositions are recorded once).
#[derive(Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AdoptSummary {
    pub crashed: usize,
    pub rearmed: usize,
    pub released: usize,
    pub swept: usize,
    pub kept: usize,
}

/// Reconcile the session registry against reality (spec §8.5: run
/// automatically at campd start, available as `camp adopt`). Observation
/// over state: the process table (uuid probe) is the ground truth, never
/// the registry row alone. Per live row not already tracked (campd's own
/// children included — round-1 minor 4): dead → session.crashed (the fold
/// releases beads, budgets intact); alive with an open bead → re-arm as
/// AdoptedPid; alive with its bead closed/absent and campd-spawned →
/// release (kill + reasoned stop; a finished stream worker lingers by P3).
/// Then sweep <camp>/worktrees/ by the Decision G table.
/// §5.3.4: the named, greppable cause when campd kills a worker it can no
/// longer answer (an unanswered permission with no live stdin).
pub(super) const ADOPTION_PERMISSION_REASON: &str = "adoption: unanswerable permission request";

/// §5.3.4: a worker campd cannot answer (no live stdin) with an unanswered
/// permission. Kill it and record the NAMED, greppable crash. The fold reopens
/// the bead (session_ended-on-crash reopens `claimed_by` beads), so it becomes
/// dispatchable to a fresh worker — no observe/Respawn is involved (the worker
/// is not, or no longer, tracked for a Respawn; the fold crash-reopen is the
/// mechanism). The key MUST be `"name"`: `SessionEnd` is `deny_unknown_fields`,
/// so a `"session"` key would fail loud at append.
pub(super) fn crash_unanswerable_permission(
    ledger: &mut Ledger,
    session: &str,
    rig: Option<String>,
    pid: i64,
    exec_timeout: std::time::Duration,
) -> Result<()> {
    kill_pid(pid, exec_timeout)?;
    ledger.append(EventInput {
        kind: EventType::SessionCrashed,
        rig,
        actor: "campd".into(),
        bead: None,
        data: serde_json::json!({ "name": session, "reason": ADOPTION_PERMISSION_REASON }),
    })?;
    Ok(())
}

/// §5.3.4 steady state: kill any BLOCKED, campd-woke, NON-child worker campd can
/// no longer answer — a `can_use_tool` discovered via tailing AFTER adoption (an
/// adopted worker holds no campd stdin). It takes the SAME named kill as the
/// startup adoption path (`crash_unanswerable_permission`), never the generic
/// stall ladder; the fold re-hooks its bead. The pid is found by PROBE (the
/// `session.woke` event carries none — it is appended before spawn), exactly as
/// `adopt` does. Returns how many were killed (drives the wake's settle).
pub(super) fn kill_discovered_unanswerable_permissions(
    ledger: &mut Ledger,
    patrol: &PatrolRuntime,
    dispatcher: &Dispatcher,
) -> Result<usize> {
    let config = &patrol.camp_config;
    let exec_timeout = config.dispatch.exec_timeout()?;
    let mut killed = 0;
    for session in ledger.blocked_sessions()? {
        if dispatcher.is_child(&session) {
            continue; // answerable — campd holds its stdin
        }
        let Some(row) = ledger.session_by_name(&session)? else {
            continue;
        };
        if row.woke_actor != "campd" {
            continue; // never kill an attended/hook-registered session (§10)
        }
        let Some(pid) = probe_alive(
            row.claude_session_id.as_deref(),
            row.pid,
            &dispatcher.known_pids(),
            &config.dispatch.command,
            exec_timeout,
        )?
        else {
            continue; // already dead/reaped — nothing to kill
        };
        crash_unanswerable_permission(ledger, &row.name, row.rig.clone(), pid, exec_timeout)?;
        killed += 1;
    }
    Ok(killed)
}

pub fn adopt(
    ledger: &mut Ledger,
    patrol: &mut PatrolRuntime,
    dispatcher: &mut Dispatcher,
) -> Result<AdoptSummary> {
    // The camp root and config ride with the patrol runtime (loaded at
    // campd start): a parse-only config has no root and cannot adopt.
    let config = patrol.camp_config.clone();
    let root = config
        .root
        .clone()
        .context("adoption needs the camp root (config was parsed, not loaded)")?;
    let camp = crate::campdir::CampDir { root };
    let config = &config;
    let camp = &camp;
    // Every probe/kill/git subprocess below runs inline on the event loop
    // and is bounded by [dispatch] exec_timeout (issue #55).
    let exec_timeout = config.dispatch.exec_timeout()?;
    let mut summary = AdoptSummary::default();
    let now = Timestamp::now();
    for row in ledger.live_sessions()? {
        if patrol.is_tracked(&row.name) || dispatcher.is_child(&row.name) {
            continue; // already under patrol/parentage: nothing to reconcile
        }
        // A hook-registered attended session (the operator's own control
        // session: woke_actor is a hook, not "campd", and there is no pid)
        // cannot be probed by the worker-command model — and spec §10
        // forbids campd crashing/killing a session in the user's TUI. Keep
        // it live; its SessionEnd hook reconciles it. Only campd-owned
        // workers and pid-bearing rows are probed for liveness.
        //
        // KNOWN LIMITATION (review LOW 1, follow-up filed): if the TUI dies
        // WITHOUT its SessionEnd firing (kill -9, crash, power loss), this
        // row stays live forever — adopt skips it, patrol never tracks it
        // (no bead), nothing reaps it, so it lingers in `camp top` /
        // `/status`. A bounded reaper is deferred: campd has no reliable
        // liveness signal for an unattributable interactive process (no pid;
        // transcript mtime conflates idle-but-alive with dead, and a grace
        // window is needed to avoid the transcript-creation race), and
        // marking a live-but-idle attended session "stopped" has §10/UX
        // tradeoffs that warrant dedicated design rather than a rushed fix.
        // Attended registry liveness is therefore best-effort, keyed on the
        // SessionEnd hook.
        if row.woke_actor != "campd" && row.pid.is_none() {
            continue;
        }
        let alive = probe_alive(
            row.claude_session_id.as_deref(),
            row.pid,
            &dispatcher.known_pids(),
            &config.dispatch.command,
            exec_timeout,
        )?;
        match alive {
            None => {
                ledger.append(EventInput {
                    kind: EventType::SessionCrashed,
                    rig: row.rig.clone(),
                    actor: "campd".into(),
                    bead: None,
                    data: serde_json::json!({
                        "name": row.name,
                        "reason": "adopt: process not found",
                    }),
                })?;
                summary.crashed += 1;
            }
            Some(pid) => {
                // §5.3.4: a live adopted worker with an UNANSWERED permission is
                // a worker campd can no longer answer — it holds no stdin pipe
                // for this campd life, so its request would block forever. Kill
                // it with the named cause and let the fold re-hook its bead. The
                // `woke_actor == "campd"` guard mirrors the release-kill below
                // (§10: never kill in the TUI — an attended session never gets
                // --permission-prompt-tool, so it never folds a permission.pending,
                // but the guard is the defensive belt-and-braces). The §5.3
                // ledger-before-pipe ordering proves pending ⇒ the response was
                // never sent, so this never kills an ANSWERED worker.
                if row.woke_actor == "campd"
                    && ledger.pending_permission_for_session(&row.name)?.is_some()
                {
                    crash_unanswerable_permission(
                        ledger,
                        &row.name,
                        row.rig.clone(),
                        pid,
                        exec_timeout,
                    )?;
                    summary.crashed += 1;
                    continue; // do NOT adopt_from_row: it is dead; the fold reopened its bead
                }
                let bead_open = match row.bead.as_deref() {
                    Some(bead) => ledger.get_bead(bead)?.is_some_and(|b| b.status != "closed"),
                    None => false,
                };
                if bead_open && row.transcript_path.is_some() {
                    patrol.adopt_from_row(&row, pid);
                    summary.rearmed += 1;
                } else if row.woke_actor == "campd" {
                    // A finished-but-lingering stream worker (P3): the
                    // release rule, non-child flavor. Never applied to
                    // attended sessions (spec §10: never kill in the TUI).
                    kill_pid(pid, exec_timeout)?;
                    // Classification by RE-PROBE (round-2 LOW 4): only an
                    // observed death earns the stop record; a kill that
                    // did not take is a durable degradation and the row
                    // stays live for the next adopt.
                    if probe_alive(
                        row.claude_session_id.as_deref(),
                        row.pid,
                        &dispatcher.known_pids(),
                        &config.dispatch.command,
                        exec_timeout,
                    )?
                    .is_some()
                    {
                        ledger.append(EventInput {
                            kind: EventType::PatrolDegraded,
                            rig: None,
                            actor: "campd".into(),
                            bead: None,
                            data: serde_json::json!({
                                "error": format!(
                                    "adopt release kill of pid {pid} did not take"
                                ),
                                "session": row.name,
                            }),
                        })?;
                        continue;
                    }
                    ledger.append(EventInput {
                        kind: EventType::SessionStopped,
                        rig: row.rig.clone(),
                        actor: "campd".into(),
                        bead: None,
                        data: serde_json::json!({
                            "name": row.name,
                            "reason": "released after bead close",
                        }),
                    })?;
                    summary.released += 1;
                }
                // attended + no open bead: not patrol's business
            }
        }
    }
    patrol.apply_tracking(ledger, now)?;
    sweep_worktrees(ledger, camp, config, exec_timeout, &mut summary)?;
    Ok(summary)
}

/// The Decision G sweep table: complete interrupted dispositions, never
/// delete what camp cannot attribute, leave in-use/reusable worktrees be.
fn sweep_worktrees(
    ledger: &mut Ledger,
    camp: &crate::campdir::CampDir,
    config: &CampConfig,
    exec_timeout: std::time::Duration,
    summary: &mut AdoptSummary,
) -> Result<()> {
    let worktrees = camp.worktrees_path();
    let entries = match std::fs::read_dir(&worktrees) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(e).with_context(|| format!("reading {}", worktrees.display()));
        }
    };
    for entry in entries {
        let entry = entry.with_context(|| format!("reading {}", worktrees.display()))?;
        if !entry.path().is_dir() {
            continue;
        }
        let bead_id = entry.file_name().to_string_lossy().into_owned();
        let Some(bead) = ledger.get_bead(&bead_id)? else {
            // Unattributable residue: report loudly, never delete.
            eprintln!(
                "campd adopt: worktree {} matches no bead; left in place",
                entry.path().display()
            );
            continue;
        };
        if bead.status != "closed" {
            continue; // in use, or awaiting re-dispatch (reused, Decision H)
        }
        // already disposed? (idempotency: one disposition per bead)
        let disposed = ledger.events_for_bead(&bead_id)?.iter().any(|e| {
            matches!(
                e.kind,
                EventType::WorktreeKept | EventType::BeadWorktreeReaped
            )
        });
        if disposed {
            continue;
        }
        if bead.outcome.as_deref() == Some("pass") {
            // complete the interrupted decision-H disposition: remove
            let removal = match config.rig(&bead.rig) {
                Ok(rig) => {
                    crate::daemon::spawn::remove_worktree(&rig.path, &entry.path(), exec_timeout)
                }
                Err(e) => Err(anyhow::anyhow!("rig not configured: {e}")),
            };
            match removal {
                Ok(()) => {
                    ledger.append(EventInput {
                        kind: EventType::BeadWorktreeReaped,
                        rig: Some(bead.rig.clone()),
                        actor: "campd".into(),
                        bead: Some(bead_id.clone()),
                        data: serde_json::json!({ "path": entry.path() }),
                    })?;
                    summary.swept += 1;
                }
                Err(e) => {
                    ledger.append(EventInput {
                        kind: EventType::WorktreeKept,
                        rig: Some(bead.rig.clone()),
                        actor: "campd".into(),
                        bead: Some(bead_id.clone()),
                        data: serde_json::json!({
                            "path": entry.path(),
                            "reason": format!("adopt: removal failed: {e:#}"),
                        }),
                    })?;
                    summary.kept += 1;
                }
            }
        } else {
            ledger.append(EventInput {
                kind: EventType::WorktreeKept,
                rig: Some(bead.rig.clone()),
                actor: "campd".into(),
                bead: Some(bead_id.clone()),
                data: serde_json::json!({
                    "path": entry.path(),
                    "reason": "adopt: found after interrupted disposition; kept for forensics",
                }),
            })?;
            summary.kept += 1;
        }
    }
    Ok(())
}

/// The mechanical status-request turn (machinery like WORKER_CONTRACT,
/// zero role content).
const NUDGE_PROMPT: &str = "Camp patrol status request: no activity has been observed for \
{threshold}. Bead {bead} is still open. If you are mid-task, continue and record a milestone: \
`camp event emit \"<one line>\" --bead {bead} --session {session}`. If the work is finished, \
close it now with `camp close {bead} --outcome <pass|fail> --reason \"<one line>\"` and exit.";

fn nudge_text(bead: &str, session: &str, threshold: &str) -> String {
    NUDGE_PROMPT
        .replace("{bead}", bead)
        .replace("{session}", session)
        .replace("{threshold}", threshold)
}

/// Probe whether the session's process is alive — OBSERVATION over state
/// (spec §8.5): match the pre-assigned claude session uuid against the
/// process table (`pgrep -f`, uuid-unique and pid-reuse-immune), excluding
/// pids campd itself owns (a nudge-resume aux child carries the uuid in
/// its argv) and pids that are not the worker command (round-2 LOW 3: an
/// operator's `tail -f <transcript>` carries the uuid too — it must never
/// be probe-identified, let alone killed). Falls back to `ps -p` for rows
/// that recorded a pid but no uuid. Neither identity ⇒ unobservable ⇒ not
/// observed alive. A missing probe binary is a hard error — fail fast.
pub(super) fn probe_alive(
    claude_session_id: Option<&str>,
    pid: Option<i64>,
    exclude: &HashSet<u32>,
    worker_command: &Path,
    timeout: std::time::Duration,
) -> Result<Option<i64>> {
    if let Some(uuid) = claude_session_id {
        let out = crate::daemon::bounded::output_bounded(
            std::process::Command::new("pgrep").arg("-f").arg(uuid),
            timeout,
        )
        .context("running pgrep (required for adoption probes)")?;
        return match out.status.code() {
            Some(0) => {
                let candidates: Vec<i64> = String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .filter_map(|l| l.trim().parse::<i64>().ok())
                    .filter(|p| {
                        u32::try_from(*p)
                            .map(|p| !exclude.contains(&p))
                            .unwrap_or(true)
                    })
                    .collect();
                for candidate in candidates {
                    if pid_runs_command(candidate, worker_command, timeout)? {
                        return Ok(Some(candidate));
                    }
                }
                Ok(None)
            }
            Some(1) => Ok(None), // no match: not observed alive
            _ => anyhow::bail!(
                "pgrep failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        };
    }
    if let Some(pid) = pid {
        let out = crate::daemon::bounded::output_bounded(
            std::process::Command::new("ps").args(["-p", &pid.to_string(), "-o", "pid="]),
            timeout,
        )
        .context("running ps (required for adoption probes)")?;
        if out.status.success() && !String::from_utf8_lossy(&out.stdout).trim().is_empty() {
            return Ok(Some(pid));
        }
        return Ok(None);
    }
    Ok(None)
}

/// Whether the pid's command line names the configured worker command in
/// its leading argv tokens: token 0 for direct execs (`claude …`, a
/// script run by path), token 1 for shebang/interpreter execs (`bash
/// /path/fake-agent.sh …`). Deliberately biased toward UNDER-matching
/// (paths with spaces mis-tokenize): a probe miss degrades to
/// "found dead" — a visible respawn in the ledger — never to killing an
/// innocent process. A pid that vanished mid-probe is simply not the
/// worker anymore.
fn pid_runs_command(pid: i64, worker_command: &Path, timeout: std::time::Duration) -> Result<bool> {
    let Some(want) = worker_command.file_name() else {
        return Ok(false);
    };
    let out = crate::daemon::bounded::output_bounded(
        std::process::Command::new("ps").args(["-p", &pid.to_string(), "-o", "command="]),
        timeout,
    )
    .context("running ps (required for adoption probes)")?;
    if !out.status.success() {
        return Ok(false); // died between pgrep and ps: not observed alive
    }
    let cmdline = String::from_utf8_lossy(&out.stdout);
    Ok(cmdline
        .split_whitespace()
        .take(2)
        .any(|token| Path::new(token).file_name() == Some(want)))
}

/// Attempt to terminate a NON-child process by pid, via /bin/kill (no
/// unsafe, no new deps — the master plan's sanctioned `ps`/`kill` route).
/// The kill's OWN exit status is deliberately not consulted (round-2 LOW
/// 4): the process may have exited in the probe-to-kill window (accepted,
/// ms-scale) and stderr text is platform/locale-dependent — every caller
/// classifies the outcome by RE-PROBING, which is the observation the
/// ledger record rests on anyway. Only a missing/unrunnable kill binary
/// is an error (fail fast).
fn kill_pid(pid: i64, timeout: std::time::Duration) -> Result<()> {
    crate::daemon::bounded::output_bounded(
        std::process::Command::new("kill")
            .arg("-9")
            .arg(pid.to_string()),
        timeout,
    )
    .context("running kill")?;
    Ok(())
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

    /// Generous test bound: these fixtures exercise probe/kill semantics,
    /// not the deadline (bounded.rs pins the deadline behavior).
    const TEST_EXEC_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    /// A camp root with a ledger, one rig, and a `dev` agent definition.
    fn fixture() -> (tempfile::TempDir, Ledger, CampConfig, PatrolRuntime) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("camp.toml"),
            "[camp]\nname = \"t\"\n\n[[rigs]]\nname = \"gc\"\npath = \"/tmp\"\nprefix = \"gc\"\n\n\
             [agent_defaults]\ntools = [\"Read\"]\n",
        )
        .unwrap();
        // Directory agents (umbrella §5.1): identity is the directory name;
        // model/tools/permission are operator-owned via [agent_defaults].
        let dev = dir.path().join("agents/dev");
        std::fs::create_dir_all(&dev).unwrap();
        std::fs::write(dev.join("prompt.md"), "Work.\n").unwrap();
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

    /// A worker.milestone attributed to `session` (actor = the session
    /// name) — `observe` counts it as worker activity, resetting the timer.
    fn milestone_event(ledger: &mut Ledger, session: &str, bead: &str) -> Event {
        let seq = ledger
            .append(EventInput {
                kind: EventType::WorkerMilestone,
                rig: Some("gc".into()),
                actor: session.into(),
                bead: Some(bead.into()),
                data: serde_json::json!({ "text": "progress" }),
            })
            .unwrap();
        ledger.events_range(seq, Some(seq)).unwrap().remove(0)
    }

    /// Close a tracked bead (open → closed) — the event `observe` turns into a
    /// Release. No prior claim needed: `bead.closed` accepts any non-closed
    /// status.
    fn close_bead(ledger: &mut Ledger, bead: &str) {
        ledger
            .append(EventInput {
                kind: EventType::BeadClosed,
                rig: Some("gc".into()),
                actor: "cli".into(),
                bead: Some(bead.into()),
                data: serde_json::json!({ "outcome": "pass" }),
            })
            .unwrap();
    }

    /// The worker's drain-ack (as the shim appends it: actor `gc-shim`,
    /// `{session}`) — the event `observe` turns into a prompt KillReleased.
    fn drain_ack(ledger: &mut Ledger, session: &str) {
        ledger
            .append(EventInput {
                kind: EventType::WorkerDrainAcked,
                rig: None,
                actor: "gc-shim".into(),
                bead: None,
                data: serde_json::json!({ "session": session }),
            })
            .unwrap();
    }

    fn last_event(ledger: &Ledger) -> Event {
        let mut all = ledger.events_range(1, None).unwrap();
        all.pop().unwrap()
    }

    #[test]
    fn stalled_count_counts_stalled_workers_and_clears_on_activity() {
        let (dir, mut ledger, _config, mut patrol) = fixture();
        let transcript = dir.path().join("projects/-p/sid.jsonl");
        let woke = woke_event(&mut ledger, "t/dev/1", "dev", "gc-1", &transcript, "campd");
        patrol.observe(&woke);
        patrol
            .apply_tracking(&mut ledger, ts("2026-07-07T07:00:00Z"))
            .unwrap();
        assert_eq!(
            patrol.stalled_count(),
            0,
            "a freshly tracked worker is not stalled"
        );

        // stall timer fires (600s default) → nudge declared → worker is red
        let fires = patrol.fire_due(ts("2026-07-07T07:10:00Z"));
        patrol
            .declare_stalls(&mut ledger, &fires, ts("2026-07-07T07:10:00Z"))
            .unwrap();
        assert_eq!(patrol.stalled_count(), 1, "a stalled worker counts red");

        // a ledger event from the worker resets its timer → cleared
        let beat = milestone_event(&mut ledger, "t/dev/1", "gc-1");
        patrol.observe(&beat);
        patrol
            .apply_tracking(&mut ledger, ts("2026-07-07T07:11:00Z"))
            .unwrap();
        assert_eq!(
            patrol.stalled_count(),
            0,
            "worker activity clears the stalled flag"
        );
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
        let canonical = transcript
            .parent()
            .unwrap()
            .canonicalize()
            .unwrap()
            .join(transcript.file_name().unwrap());
        assert!(
            patrol
                .filter_slot()
                .lock()
                .unwrap()
                .registered
                .contains(&canonical),
            "the CANONICAL transcript path is registered (watch backends \
             report canonical paths)"
        );
        // default threshold: fires at stall_after (10m), not before
        assert!(patrol.fire_due(ts("2026-07-07T07:09:59Z")).is_empty());
        assert_eq!(patrol.fire_due(ts("2026-07-07T07:10:00Z")).len(), 1);
    }

    #[test]
    fn frontmatter_stall_after_governs_the_armed_threshold() {
        let (dir, mut ledger, _config, mut patrol) = fixture();
        // round-1 review note: the 5m override must actually arm at 5m.
        // The override now lives in the agent DIRECTORY's agent.toml.
        let quick = dir.path().join("agents/quick");
        std::fs::create_dir_all(&quick).unwrap();
        std::fs::write(quick.join("agent.toml"), "stall_after = \"5m\"\n").unwrap();
        std::fs::write(quick.join("prompt.md"), "Work fast.\n").unwrap();
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
        let t0 = ts("2026-07-07T07:00:00Z");
        // Three distinct workers, each armed once at t0 (10m default →
        // deadline 07:10) — one per Decision-J key, so no artificial
        // re-arm is needed (the round-2 fix-2 guard skips a re-Track of an
        // already-tracked session, which the old single-session re-observe
        // relied on).
        for (name, bead) in [
            ("t/dev/1", "gc-1"),
            ("t/dev/2", "gc-2"),
            ("t/dev/3", "gc-3"),
        ] {
            let transcript = dir.path().join(format!("projects/-p/{bead}.jsonl"));
            let event = woke_event(&mut ledger, name, "dev", bead, &transcript, "campd");
            patrol.observe(&event);
        }
        patrol.apply_tracking(&mut ledger, t0).unwrap();

        let ev = |l: &mut Ledger,
                  kind: EventType,
                  bead: Option<&str>,
                  actor: &str,
                  data: serde_json::Value|
         -> Event {
            let seq = l
                .append(EventInput {
                    kind,
                    rig: Some("gc".into()),
                    actor: actor.into(),
                    bead: bead.map(str::to_owned),
                    data,
                })
                .unwrap();
            l.events_range(seq, Some(seq)).unwrap().remove(0)
        };
        // (a) bead match: worker.milestone --bead gc-1  (resets t/dev/1)
        let a = ev(
            &mut ledger,
            EventType::WorkerMilestone,
            Some("gc-1"),
            "cli",
            serde_json::json!({"text": "progress"}),
        );
        // (b) actor == session name: event emit --session (actor t/dev/2)
        let b = ev(
            &mut ledger,
            EventType::WorkerMilestone,
            None,
            "t/dev/2",
            serde_json::json!({"text": "note"}),
        );
        // (c) data.session == session name: bead.claimed session t/dev/3
        let c = ev(
            &mut ledger,
            EventType::BeadClaimed,
            Some("gc-3"),
            "cli",
            serde_json::json!({"session": "t/dev/3"}),
        );
        for e in [&a, &b, &c] {
            patrol.observe(e);
        }
        patrol
            .apply_tracking(&mut ledger, ts("2026-07-07T07:05:00Z"))
            .unwrap();

        // every old deadline (07:10) is gone; every pushed one fires at 07:15
        assert!(
            patrol.fire_due(ts("2026-07-07T07:10:00Z")).is_empty(),
            "all three deadlines must be pushed past 07:10"
        );
        assert_eq!(
            patrol.fire_due(ts("2026-07-07T07:15:00Z")).len(),
            3,
            "each Decision-J key pushed its session's deadline to 07:15"
        );
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

        // the notify callback observed activity on the registered path —
        // reported CANONICALIZED, as the real backends do...
        let canonical = transcript
            .parent()
            .unwrap()
            .canonicalize()
            .unwrap()
            .join(transcript.file_name().unwrap());
        let mut hit = notify::Event::new(notify::EventKind::Any);
        hit.paths.push(canonical);
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
        assert!(
            patrol
                .declare_stalls(&mut ledger, &fires, ts("2026-07-07T07:10:00Z"))
                .unwrap()
        );
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
        patrol
            .declare_stalls(&mut ledger, &fires, ts("2026-07-07T07:20:00Z"))
            .unwrap();
        let stalled = stalled_events(&ledger);
        assert_eq!(stalled.len(), 2);
        assert_eq!(stalled[1].data["action"], "restart");
        assert_eq!(stalled[1].data["restarts"], 1);
    }

    /// A pending permission for `session` in the ledger (the BLOCKED marker).
    fn seed_pending(ledger: &mut Ledger, session: &str, request_id: &str) {
        ledger
            .append(EventInput {
                kind: EventType::PermissionPending,
                rig: None,
                actor: "campd".into(),
                bead: None,
                data: serde_json::json!({
                    "session": session, "request_id": request_id, "tool_name": "Bash",
                }),
            })
            .unwrap();
    }

    /// cp-3 §5.3.3 unit guard 1 (INVARIANT 1): a BLOCKED session must contribute
    /// NOTHING to the poll deadline. reconcile_blocked DISARMS its stall timer on
    /// the working→blocked edge. Mutation caught: dropping reconcile_blocked's
    /// disarm leaves the timer armed → RED.
    #[test]
    fn permission_pending_disarms_the_stall_timer_so_a_blocked_worker_adds_no_wakeup() {
        let (dir, mut ledger, _config, mut patrol) = fixture();
        let transcript = dir.path().join("projects/-p/sid.jsonl");
        let event = woke_event(&mut ledger, "t/dev/1", "dev", "gc-1", &transcript, "campd");
        let now = ts("2026-07-07T07:00:00Z");
        patrol.observe(&event);
        patrol.apply_tracking(&mut ledger, now).unwrap();
        assert!(
            patrol.is_armed("t/dev/1"),
            "precondition: an armed stall timer"
        );

        seed_pending(&mut ledger, "t/dev/1", "cli-1");
        patrol.reconcile_blocked(&ledger, now).unwrap();
        assert!(
            !patrol.is_armed("t/dev/1"),
            "permission.pending DISARMS — a blocked worker adds no wakeup (invariant 1)"
        );
    }

    /// cp-3 §5.3.3 unit guard 2 (THE SKIP, exercised alone): feed declare_stalls
    /// a synthetic Stall fire for a session that is in patrol.blocked WITH its
    /// timer still armed. It must declare ZERO agent.stalled and leave the timer
    /// disarmed. Mutation caught: removing the `self.blocked.contains` skip in
    /// declare_stalls → the fire declares a stall → RED.
    #[test]
    fn declare_stalls_declares_nothing_for_a_blocked_session_even_with_an_armed_timer() {
        let (dir, mut ledger, _config, mut patrol) = fixture();
        let transcript = dir.path().join("projects/-p/sid.jsonl");
        let event = woke_event(&mut ledger, "t/dev/1", "dev", "gc-1", &transcript, "campd");
        let now = ts("2026-07-07T07:00:00Z");
        patrol.observe(&event);
        patrol.apply_tracking(&mut ledger, now).unwrap();
        assert!(
            patrol.is_armed("t/dev/1"),
            "precondition: an armed stall timer"
        );
        // Mark it blocked WITHOUT disarming (this guard isolates the skip, not
        // the reconcile disarm — the private field is reachable from this child
        // module).
        patrol.blocked.insert("t/dev/1".to_owned());

        let fire = StallFire {
            session: "t/dev/1".into(),
            kind: TimerKind::Stall,
            deadline: now,
            threshold: SignedDuration::from_secs(600),
        };
        let declared = patrol.declare_stalls(&mut ledger, &[fire], now).unwrap();
        assert!(!declared, "a blocked session declares nothing");
        assert_eq!(
            ledger
                .events_of_type(EventType::AgentStalled)
                .unwrap()
                .len(),
            0
        );
        assert!(
            !patrol.is_armed("t/dev/1"),
            "the swallowed fire disarms the timer"
        );
    }

    /// cp-3 §5.3.3 unit guard 3 (THE RE-ARM edge): a blocked-then-decided session
    /// gets a fresh armed timer. Mutation caught: dropping reconcile_blocked's
    /// re-arm leaves the decided worker un-timed → RED.
    #[test]
    fn a_decision_re_arms_the_stall_timer_from_zero() {
        let (dir, mut ledger, _config, mut patrol) = fixture();
        let transcript = dir.path().join("projects/-p/sid.jsonl");
        let event = woke_event(&mut ledger, "t/dev/1", "dev", "gc-1", &transcript, "campd");
        let now = ts("2026-07-07T07:00:00Z");
        patrol.observe(&event);
        patrol.apply_tracking(&mut ledger, now).unwrap();

        // block it (disarms), then decide it (a decided permission is no longer
        // in blocked_sessions).
        seed_pending(&mut ledger, "t/dev/1", "cli-1");
        patrol.reconcile_blocked(&ledger, now).unwrap();
        assert!(!patrol.is_armed("t/dev/1"), "blocked → disarmed");
        ledger
            .append(EventInput {
                kind: EventType::PermissionDecided,
                rig: None,
                actor: "campd".into(),
                bead: None,
                data: serde_json::json!({
                    "session": "t/dev/1", "request_id": "cli-1",
                    "decision": "allow", "decided_by": "operator",
                }),
            })
            .unwrap();
        patrol.reconcile_blocked(&ledger, now).unwrap();
        assert!(
            patrol.is_armed("t/dev/1"),
            "a decision re-arms from zero — the worker is presumed working again"
        );
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
        patrol
            .declare_stalls(&mut ledger, &fires, ts("2026-07-07T07:10:00Z"))
            .unwrap();
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
        patrol
            .declare_stalls(&mut ledger, &fires, ts("2026-07-07T07:20:00Z"))
            .unwrap();
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
        patrol
            .declare_stalls(&mut ledger, &fires, ts("2026-07-07T07:12:00Z"))
            .unwrap(); // nudge
        let fires = patrol.fire_due(ts("2026-07-07T07:22:00Z"));
        patrol
            .declare_stalls(&mut ledger, &fires, ts("2026-07-07T07:22:00Z"))
            .unwrap(); // exhausted
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
            patrol.declare_stalls(&mut ledger, &fires, ts(at)).unwrap();
        }
        for e in stalled_events(&ledger) {
            assert_eq!(
                e.data["action"], "annotate",
                "attended sessions annotate only, never nudge/restart"
            );
        }
    }

    // ---- Task 11.11: action execution (nudge / restart / release) --------

    use crate::daemon::dispatch::Dispatcher;

    fn dispatcher_for(dir: &std::path::Path, config: &CampConfig) -> Dispatcher {
        Dispatcher::new(
            crate::campdir::CampDir {
                root: dir.to_path_buf(),
            },
            config.clone(),
        )
    }

    /// Track a session in patrol from a woke event and arm it.
    fn track(patrol: &mut PatrolRuntime, ledger: &mut Ledger, event: &Event, now: &str) {
        patrol.observe(event);
        patrol.apply_tracking(ledger, ts(now)).unwrap();
    }

    #[test]
    fn a_child_nudge_goes_over_stdin_and_a_pipeless_one_resumes() {
        let (dir, mut ledger, mut config, _p) = fixture();
        // a recording stand-in for the worker command (resume half): it
        // writes argv+cwd RELATIVE to its cwd, which pins that the resume
        // child runs in the worker's worktree.
        let recorder = dir.path().join("recorder.sh");
        std::fs::write(
            &recorder,
            "#!/bin/bash\nprintf '%s\\n' \"$@\" > resume-args.txt\npwd >> resume-args.txt\n",
        )
        .unwrap();
        std::fs::set_permissions(
            &recorder,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        config.dispatch.command = recorder;
        let patrol_config = camp_core::patrol::PatrolConfig::from_section(&config.patrol).unwrap();
        let mut patrol = PatrolRuntime::new(patrol_config, &config);
        let mut dispatcher = dispatcher_for(dir.path(), &config);

        // CHILD half: a held cat under the tracked session name
        let transcript = dir.path().join("projects/-p/sid.jsonl");
        let event = woke_event(&mut ledger, "t/dev/1", "dev", "gc-1", &transcript, "campd");
        track(&mut patrol, &mut ledger, &event, "2026-07-07T07:00:00Z");
        let pid = dispatcher.test_insert_held_cat(dir.path(), "t/dev/1", "gc-1");
        patrol.pending.push(PendingAction::Nudge {
            session: "t/dev/1".into(),
            cause_seq: 1,
        });
        patrol
            .execute_pending(&mut ledger, &mut dispatcher, ts("2026-07-07T07:10:00Z"))
            .unwrap();
        // read the delivered line: release the pipe so cat exits
        dispatcher.release_worker("gc-1", "test readback");
        dispatcher.test_child_wait(pid);
        let delivered = std::fs::read_to_string(dir.path().join("gc-1.out")).unwrap();
        let v: serde_json::Value = serde_json::from_str(delivered.trim_end()).unwrap();
        assert_eq!(v["type"], "user");
        let text = v["message"]["content"].as_str().unwrap();
        assert!(
            text.contains("gc-1") && text.contains("t/dev/1") && text.contains("camp close"),
            "the nudge text is the mechanical status request: {text}"
        );
        assert!(
            stalled_events(&ledger).is_empty(),
            "a delivered nudge appends nothing further"
        );

        // PIPELESS half: an adopted session resumes via the worker
        // command, in the worker's worktree cwd.
        let worktree = dir.path().join("wt-gc-2");
        std::fs::create_dir_all(&worktree).unwrap();
        seeded_bead(&mut ledger, "gc-2");
        let seq = ledger
            .append(EventInput {
                kind: EventType::SessionWoke,
                rig: Some("gc".into()),
                actor: "campd".into(),
                bead: Some("gc-2".into()),
                data: serde_json::json!({
                    "name": "t/dev/2", "agent": "dev",
                    "claude_session_id": "11111111-2222-4333-8444-555555555555",
                    "transcript_path": dir.path().join("projects/-p/sid2.jsonl"),
                    "bead": "gc-2",
                    "worktree": worktree,
                }),
            })
            .unwrap();
        let event2 = ledger.events_range(seq, Some(seq)).unwrap().remove(0);
        track(&mut patrol, &mut ledger, &event2, "2026-07-07T07:00:00Z");
        patrol.pending.push(PendingAction::Nudge {
            session: "t/dev/2".into(),
            cause_seq: 2,
        });
        patrol
            .execute_pending(&mut ledger, &mut dispatcher, ts("2026-07-07T07:10:00Z"))
            .unwrap();
        let record = worktree.join("resume-args.txt");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !record.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "resume child never ran"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        std::thread::sleep(std::time::Duration::from_millis(50)); // let it finish writing
        let args = std::fs::read_to_string(&record).unwrap();
        assert!(args.contains("--resume"), "args: {args}");
        assert!(
            args.contains("11111111-2222-4333-8444-555555555555"),
            "the resume names the claude session id: {args}"
        );
        assert!(
            args.contains("wt-gc-2"),
            "the resume child runs in the worker's worktree: {args}"
        );
        assert!(
            !args.contains("--allowedTools") && !args.contains("--permission-mode"),
            "a woke without pins resumes bare (a recorded absence): {args}"
        );
    }

    #[test]
    fn a_failed_nudge_is_evented_and_advances_the_ladder() {
        let (dir, mut ledger, config, mut patrol) = fixture();
        let mut dispatcher = dispatcher_for(dir.path(), &config);
        // a campd-owned session with NO held child and NO claude session
        // id: both nudge paths are impossible -> nudge_failed
        seeded_bead(&mut ledger, "gc-3");
        let seq = ledger
            .append(EventInput {
                kind: EventType::SessionWoke,
                rig: Some("gc".into()),
                actor: "campd".into(),
                bead: Some("gc-3".into()),
                data: serde_json::json!({
                    "name": "t/dev/3", "agent": "dev",
                    "transcript_path": dir.path().join("projects/-p/sid3.jsonl"),
                    "bead": "gc-3",
                }),
            })
            .unwrap();
        let event = ledger.events_range(seq, Some(seq)).unwrap().remove(0);
        track(&mut patrol, &mut ledger, &event, "2026-07-07T07:00:00Z");

        patrol.pending.push(PendingAction::Nudge {
            session: "t/dev/3".into(),
            cause_seq: seq,
        });
        patrol
            .execute_pending(&mut ledger, &mut dispatcher, ts("2026-07-07T07:10:00Z"))
            .unwrap();
        let stalled = stalled_events(&ledger);
        assert_eq!(stalled.len(), 1);
        assert_eq!(stalled[0].data["action"], "nudge_failed");
        assert_eq!(stalled[0].data["via"], "resume");
        assert!(
            stalled[0].data["error"]
                .as_str()
                .unwrap()
                .contains("claude session id"),
            "{}",
            stalled[0].data
        );
        // the ladder advanced: the next fire is a restart
        let fires = patrol.fire_due(ts("2026-07-07T07:30:00Z"));
        assert_eq!(fires.len(), 1, "nudge_failed re-armed the timer");
        patrol
            .declare_stalls(&mut ledger, &fires, ts("2026-07-07T07:30:00Z"))
            .unwrap();
        assert_eq!(
            stalled_events(&ledger).last().unwrap().data["action"],
            "restart"
        );
    }

    #[test]
    fn restart_kills_the_child_and_the_crash_carries_the_cause() {
        let (dir, mut ledger, config, mut patrol) = fixture();
        let mut dispatcher = dispatcher_for(dir.path(), &config);
        let transcript = dir.path().join("projects/-p/sid.jsonl");
        let event = woke_event(&mut ledger, "t/dev/1", "dev", "gc-1", &transcript, "campd");
        track(&mut patrol, &mut ledger, &event, "2026-07-07T07:00:00Z");
        // claim it so the release-on-crash is observable
        ledger
            .append(EventInput {
                kind: EventType::BeadClaimed,
                rig: Some("gc".into()),
                actor: "cli".into(),
                bead: Some("gc-1".into()),
                data: serde_json::json!({"session": "t/dev/1"}),
            })
            .unwrap();
        let pid = dispatcher.test_insert_held_cat(dir.path(), "t/dev/1", "gc-1");

        patrol.pending.push(PendingAction::Restart {
            session: "t/dev/1".into(),
            cause_seq: 77,
        });
        patrol
            .execute_pending(&mut ledger, &mut dispatcher, ts("2026-07-07T07:10:00Z"))
            .unwrap();
        dispatcher.test_child_wait(pid);
        dispatcher.reap(&mut ledger).unwrap();
        let events = ledger.events_range(1, None).unwrap();
        let crashed = events
            .iter()
            .find(|e| e.kind.as_str() == "session.crashed")
            .expect("the restart kill must reap as crashed");
        assert_eq!(crashed.data["cause_seq"], 77);
        assert_eq!(crashed.data["reason"], "patrol restart");
        let bead = ledger.get_bead("gc-1").unwrap().unwrap();
        assert_eq!(bead.status, "open", "the fold released the bead");
    }

    /// ROUND-1 BLOCKER 2 REGRESSION PIN: an AdoptedPid restart re-probes
    /// the session uuid immediately before killing and kills the
    /// re-probed pid only — a stale pid must never translate into a
    /// SIGKILL of whatever innocent process now owns it.
    #[test]
    fn adopted_restart_reprobes_before_killing_and_never_kills_a_stale_pid() {
        let _spawning = crate::daemon::spawn_probe_guard();
        let (dir, mut ledger, config, mut patrol) = fixture();
        let mut dispatcher = dispatcher_for(dir.path(), &config);

        // Case A: the worker is long dead; its stale pid now belongs to an
        // INNOCENT process (a plain sleeper WITHOUT the uuid in argv).
        let mut innocent = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let stale_pid = i64::from(innocent.id());
        let dead_uuid = "99999999-9999-4999-8999-999999999999";
        seeded_bead(&mut ledger, "gc-4");
        let seq = ledger
            .append(EventInput {
                kind: EventType::SessionWoke,
                rig: Some("gc".into()),
                actor: "campd".into(),
                bead: Some("gc-4".into()),
                data: serde_json::json!({
                    "name": "t/dev/4", "agent": "dev",
                    "claude_session_id": dead_uuid,
                    "transcript_path": dir.path().join("projects/-p/sid4.jsonl"),
                    "bead": "gc-4",
                }),
            })
            .unwrap();
        let event = ledger.events_range(seq, Some(seq)).unwrap().remove(0);
        patrol.observe(&event);
        patrol
            .apply_tracking(&mut ledger, ts("2026-07-07T07:00:00Z"))
            .unwrap();
        // adopt-shape ownership: the pid observed hours ago
        patrol.tracked.get_mut("t/dev/4").unwrap().owned = Owned::AdoptedPid(stale_pid);

        patrol.pending.push(PendingAction::Restart {
            session: "t/dev/4".into(),
            cause_seq: 88,
        });
        patrol
            .execute_pending(&mut ledger, &mut dispatcher, ts("2026-07-07T07:10:00Z"))
            .unwrap();
        let events = ledger.events_range(1, None).unwrap();
        let crashed = events
            .iter()
            .find(|e| e.kind.as_str() == "session.crashed")
            .expect("a dead adopted worker still gets its caused crash record");
        assert!(
            crashed.data["reason"]
                .as_str()
                .unwrap()
                .contains("found dead"),
            "{}",
            crashed.data
        );
        assert_eq!(crashed.data["cause_seq"], 88);
        assert!(
            matches!(innocent.try_wait(), Ok(None)),
            "the INNOCENT process at the stale pid must still be alive"
        );
        innocent.kill().unwrap();
        innocent.wait().unwrap();

        // Case B: the adopted worker IS alive (uuid in argv): the re-probed
        // pid is killed and the caused crash recorded.
        let live_uuid = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
        // `|| true` keeps the command compound so bash stays resident
        // (a simple command would be exec-replaced, losing the uuid from
        // the process table); arg0 makes it worker-shaped for the LOW-3
        // command filter.
        use std::os::unix::process::CommandExt as _;
        let mut cmd = std::process::Command::new("bash");
        cmd.arg0("claude")
            .arg("-c")
            .arg(format!("sleep 30 || true # {live_uuid}"));
        let mut sleeper = cmd.spawn().unwrap();
        seeded_bead(&mut ledger, "gc-5");
        let seq = ledger
            .append(EventInput {
                kind: EventType::SessionWoke,
                rig: Some("gc".into()),
                actor: "campd".into(),
                bead: Some("gc-5".into()),
                data: serde_json::json!({
                    "name": "t/dev/5", "agent": "dev",
                    "claude_session_id": live_uuid,
                    "transcript_path": dir.path().join("projects/-p/sid5.jsonl"),
                    "bead": "gc-5",
                }),
            })
            .unwrap();
        let event = ledger.events_range(seq, Some(seq)).unwrap().remove(0);
        patrol.observe(&event);
        patrol
            .apply_tracking(&mut ledger, ts("2026-07-07T07:00:00Z"))
            .unwrap();
        patrol.tracked.get_mut("t/dev/5").unwrap().owned =
            Owned::AdoptedPid(i64::from(sleeper.id()));

        patrol.pending.push(PendingAction::Restart {
            session: "t/dev/5".into(),
            cause_seq: 99,
        });
        patrol
            .execute_pending(&mut ledger, &mut dispatcher, ts("2026-07-07T07:10:00Z"))
            .unwrap();
        let status = sleeper.wait().unwrap();
        assert!(!status.success(), "the live adopted worker was killed");
        let events = ledger.events_range(1, None).unwrap();
        let crashed: Vec<_> = events
            .iter()
            .filter(|e| e.kind.as_str() == "session.crashed")
            .collect();
        assert_eq!(crashed.last().unwrap().data["reason"], "patrol restart");
        assert_eq!(crashed.last().unwrap().data["cause_seq"], 99);
    }

    /// ROUND-2 HIGH 1: a worker orphaned across a campd crash — its
    /// session.woke row sits past the cursor and is replayed by the
    /// STARTUP settle, which re-tracks it Owned::Child (actor=="campd")
    /// even though the fresh dispatcher never held it — must STILL be
    /// killed on restart and released on bead-close. The action verifies
    /// against the LIVE dispatcher, never the label. Both processes are
    /// alive MID-WINDOW (the fresh campd does not hold them), which is why
    /// the existing kill-9 test — waiting for both claims before killing —
    /// misses this.
    #[test]
    fn an_orphan_retracked_as_child_is_still_restarted_and_released() {
        let _spawning = crate::daemon::spawn_probe_guard();
        let (dir, mut ledger, config, mut patrol) = fixture();
        let mut dispatcher = dispatcher_for(dir.path(), &config);
        use std::os::unix::process::CommandExt as _;

        let spawn_orphan = |uuid: &str| {
            let mut cmd = std::process::Command::new("bash");
            cmd.arg0("claude") // worker-shaped for the LOW-3 command filter
                .arg("-c")
                .arg(format!("sleep 30 || true # {uuid}"));
            cmd.spawn().unwrap()
        };
        // R: driven through Restart. C: driven through bead-close Release.
        let uuid_r = "00000000-dead-4000-8000-0000000000aa";
        let uuid_c = "00000000-dead-4000-8000-0000000000bb";
        let mut orphan_r = spawn_orphan(uuid_r);
        let mut orphan_c = spawn_orphan(uuid_c);

        // The startup settle replays their campd-actor woke rows →
        // Track{Owned::Child}, even though this fresh dispatcher holds
        // neither. Claim each bead so a crash release is observable.
        for (name, uuid, bead) in [("t/dev/R", uuid_r, "gc-1"), ("t/dev/C", uuid_c, "gc-2")] {
            seeded_bead(&mut ledger, bead);
            let seq = ledger
                .append(EventInput {
                    kind: EventType::SessionWoke,
                    rig: Some("gc".into()),
                    actor: "campd".into(),
                    bead: Some(bead.into()),
                    data: serde_json::json!({
                        "name": name, "agent": "dev",
                        "claude_session_id": uuid,
                        "transcript_path": dir.path().join(format!("projects/-p/{bead}.jsonl")),
                        "bead": bead,
                    }),
                })
                .unwrap();
            let event = ledger.events_range(seq, Some(seq)).unwrap().remove(0);
            patrol.observe(&event);
            ledger
                .append(EventInput {
                    kind: EventType::BeadClaimed,
                    rig: Some("gc".into()),
                    actor: "cli".into(),
                    bead: Some(bead.into()),
                    data: serde_json::json!({"session": name}),
                })
                .unwrap();
        }
        patrol
            .apply_tracking(&mut ledger, ts("2026-07-07T07:00:00Z"))
            .unwrap();
        // the misclassification the bug rests on: both labelled Child
        assert!(matches!(
            patrol.tracked.get("t/dev/R").unwrap().owned,
            Owned::Child
        ));
        assert!(
            !dispatcher.is_child("t/dev/R"),
            "the fresh campd holds neither"
        );

        // drive Restart on R and BeadClosed on C in the same wake
        patrol.pending.push(PendingAction::Restart {
            session: "t/dev/R".into(),
            cause_seq: 50,
        });
        let close_seq = ledger
            .append(EventInput {
                kind: EventType::BeadClosed,
                rig: Some("gc".into()),
                actor: "cli".into(),
                bead: Some("gc-2".into()),
                data: serde_json::json!({"outcome": "pass"}),
            })
            .unwrap();
        let closed = ledger
            .events_range(close_seq, Some(close_seq))
            .unwrap()
            .remove(0);
        patrol.observe(&closed); // queues Release{gc-2}
        patrol
            .execute_pending(&mut ledger, &mut dispatcher, ts("2026-07-07T07:10:00Z"))
            .unwrap();

        // R: actually killed, caused crash recorded (not a fiction)
        assert!(
            !orphan_r.wait().unwrap().success(),
            "the orphaned 'Child' R must actually be killed on restart"
        );
        let events = ledger.events_range(1, None).unwrap();
        let r_crash = events
            .iter()
            .find(|e| e.kind.as_str() == "session.crashed" && e.data["name"] == "t/dev/R")
            .expect("R's caused crash must land (not a false agent.stalled trail)");
        assert_eq!(r_crash.data["reason"], "patrol restart");
        assert_eq!(r_crash.data["cause_seq"], 50);

        // C: actually killed, reasoned stop recorded (the spec §10 release)
        assert!(
            !orphan_c.wait().unwrap().success(),
            "the orphaned 'Child' C must be released on bead close"
        );
        let c_stop = events
            .iter()
            .find(|e| e.kind.as_str() == "session.stopped" && e.data["name"] == "t/dev/C")
            .expect("C's reasoned stop must land (spec §10 release rule)");
        assert!(
            c_stop.data["reason"].as_str().unwrap().contains("released"),
            "{}",
            c_stop.data
        );
    }

    // ---- Task 11.12: adoption --------------------------------------------

    fn woke_row(
        ledger: &mut Ledger,
        name: &str,
        bead: &str,
        uuid: &str,
        transcript: &std::path::Path,
        claim: bool,
    ) {
        seeded_bead(ledger, bead);
        ledger
            .append(EventInput {
                kind: EventType::SessionWoke,
                rig: Some("gc".into()),
                actor: "campd".into(),
                bead: Some(bead.into()),
                data: serde_json::json!({
                    "name": name, "agent": "dev", "rig": "gc",
                    "claude_session_id": uuid,
                    "transcript_path": transcript,
                    "bead": bead,
                }),
            })
            .unwrap();
        if claim {
            ledger
                .append(EventInput {
                    kind: EventType::BeadClaimed,
                    rig: Some("gc".into()),
                    actor: "cli".into(),
                    bead: Some(bead.into()),
                    data: serde_json::json!({"session": name}),
                })
                .unwrap();
        }
    }

    #[test]
    fn adopt_keeps_hook_registered_attended_sessions_never_crashes_them() {
        let _spawning = crate::daemon::spawn_probe_guard();
        let (dir, mut ledger, config, mut patrol) = fixture();
        let mut dispatcher = dispatcher_for(dir.path(), &config);
        // a hook-registered attended session: non-campd actor, no bead, no
        // pid, and a transcript path that need not exist. probe_alive would
        // find no process — but spec §10 forbids crashing it.
        ledger
            .append(EventInput {
                kind: EventType::SessionWoke,
                rig: None,
                actor: "hook:session-start".into(),
                bead: None,
                data: serde_json::json!({
                    "name": "attended/S-1", "agent": "attended",
                    "claude_session_id": "S-1", "transcript_path": "/tmp/S-1.jsonl",
                }),
            })
            .unwrap();
        let summary = adopt(&mut ledger, &mut patrol, &mut dispatcher).unwrap();
        assert_eq!(
            summary.crashed, 0,
            "adopt must never crash an attended session (spec §10)"
        );
        assert_eq!(
            ledger.session_status("attended/S-1").unwrap().as_deref(),
            Some("live"),
            "the attended session stays live across adopt"
        );
    }

    #[test]
    fn adopt_marks_dead_sessions_crashed_and_releases_their_beads() {
        let _spawning = crate::daemon::spawn_probe_guard();
        let (dir, mut ledger, config, mut patrol) = fixture();
        let mut dispatcher = dispatcher_for(dir.path(), &config);
        woke_row(
            &mut ledger,
            "t/dev/1",
            "gc-1",
            "dead0000-0000-4000-8000-000000000000",
            &dir.path().join("projects/-p/dead.jsonl"),
            true,
        );
        let summary = adopt(&mut ledger, &mut patrol, &mut dispatcher).unwrap();
        assert_eq!(summary.crashed, 1);
        assert_eq!(summary.rearmed, 0);
        let events = ledger.events_range(1, None).unwrap();
        let crashed = events
            .iter()
            .find(|e| e.kind.as_str() == "session.crashed")
            .unwrap();
        assert_eq!(crashed.data["name"], "t/dev/1");
        assert!(
            crashed.data["reason"]
                .as_str()
                .unwrap()
                .contains("adopt: process not found")
        );
        let bead = ledger.get_bead("gc-1").unwrap().unwrap();
        assert_eq!(bead.status, "open", "the fold released the claimed bead");
        assert!(bead.claimed_by.is_none());
    }

    #[test]
    fn adopt_rearms_living_sessions_and_releases_finished_ones() {
        let _spawning = crate::daemon::spawn_probe_guard();
        let (dir, mut ledger, config, mut patrol) = fixture();
        let mut dispatcher = dispatcher_for(dir.path(), &config);
        // live worker, OPEN bead → re-armed
        use std::os::unix::process::CommandExt as _;
        let live_uuid = "11ve0000-0000-4000-8000-00000000aaaa";
        let mut live_cmd = std::process::Command::new("bash");
        live_cmd
            .arg0("claude")
            .arg("-c")
            .arg(format!("sleep 30 || true # {live_uuid}"));
        let mut live = live_cmd.spawn().unwrap();
        woke_row(
            &mut ledger,
            "t/dev/1",
            "gc-1",
            live_uuid,
            &dir.path().join("projects/-p/live.jsonl"),
            true,
        );
        // live worker, CLOSED bead → released (killed + reasoned stop)
        let done_uuid = "d0ne0000-0000-4000-8000-00000000bbbb";
        let mut done_cmd = std::process::Command::new("bash");
        done_cmd
            .arg0("claude")
            .arg("-c")
            .arg(format!("sleep 30 || true # {done_uuid}"));
        let mut done = done_cmd.spawn().unwrap();
        woke_row(
            &mut ledger,
            "t/dev/2",
            "gc-2",
            done_uuid,
            &dir.path().join("projects/-p/done.jsonl"),
            true,
        );
        ledger
            .append(EventInput {
                kind: EventType::BeadClosed,
                rig: Some("gc".into()),
                actor: "cli".into(),
                bead: Some("gc-2".into()),
                data: serde_json::json!({"outcome": "pass"}),
            })
            .unwrap();

        let summary = adopt(&mut ledger, &mut patrol, &mut dispatcher).unwrap();
        assert_eq!(summary.rearmed, 1, "{summary:?}");
        assert_eq!(summary.released, 1, "{summary:?}");
        assert_eq!(summary.crashed, 0, "{summary:?}");
        // the re-armed worker has a live timer
        assert!(patrol.poll_timeout(ts("2026-07-07T07:00:00Z")).is_some());
        assert!(patrol.is_tracked("t/dev/1"));
        assert!(!patrol.is_tracked("t/dev/2"));
        // the finished one was killed and stopped with the reason
        let status = done.wait().unwrap();
        assert!(!status.success(), "the finished lingerer was terminated");
        let events = ledger.events_range(1, None).unwrap();
        let stopped = events
            .iter()
            .find(|e| e.kind.as_str() == "session.stopped")
            .unwrap();
        assert_eq!(stopped.data["name"], "t/dev/2");
        assert!(
            stopped.data["reason"]
                .as_str()
                .unwrap()
                .contains("released")
        );
        live.kill().unwrap();
        live.wait().unwrap();
    }

    /// cp-3 §5.3.4: a LIVE adopted worker with an UNANSWERED permission is
    /// killed with the named, greppable cause, and its bead re-hooks via the
    /// fold crash-reopen. Mutation caught: dropping the pending check or the
    /// named append; the bead assertions die if the append uses the wrong key
    /// `"session"` (loud fold failure) or the fold reopen is bypassed.
    #[test]
    fn adoption_kills_a_worker_with_an_unanswered_permission_and_re_hooks_the_bead() {
        let _spawning = crate::daemon::spawn_probe_guard();
        let (dir, mut ledger, config, mut patrol) = fixture();
        let mut dispatcher = dispatcher_for(dir.path(), &config);
        use std::os::unix::process::CommandExt as _;
        let uuid = "b10c0000-0000-4000-8000-00000000cccc";
        let mut cmd = std::process::Command::new("bash");
        cmd.arg0("claude")
            .arg("-c")
            .arg(format!("sleep 30 || true # {uuid}"));
        let mut child = cmd.spawn().unwrap();
        woke_row(
            &mut ledger,
            "t/dev/1",
            "gc-1",
            uuid,
            &dir.path().join("projects/-p/x.jsonl"),
            true,
        );
        // the worker asked permission and no one answered
        ledger
            .append(EventInput {
                kind: EventType::PermissionPending,
                rig: None,
                actor: "campd".into(),
                bead: None,
                data: serde_json::json!({
                    "session": "t/dev/1", "request_id": "cli-2", "tool_name": "Bash",
                }),
            })
            .unwrap();

        let summary = adopt(&mut ledger, &mut patrol, &mut dispatcher).unwrap();
        assert_eq!(summary.crashed, 1);
        assert_eq!(summary.rearmed, 0);
        let crash = ledger
            .events_of_type(EventType::SessionCrashed)
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(crash.data["name"], "t/dev/1");
        assert_eq!(
            crash.data["reason"],
            "adoption: unanswerable permission request"
        );
        // the bead is DISPATCHABLE AGAIN via the fold crash-reopen
        let bead = ledger.get_bead("gc-1").unwrap().unwrap();
        assert_eq!(bead.status, "open");
        assert!(
            bead.claimed_by.is_none(),
            "reopened + unclaimed → the readiness processor re-dispatches it"
        );
        let _ = child.wait();
    }

    /// cp-3 §5.3.4 inverse window: an ANSWERED (decided) but quiet adopted worker
    /// is NOT killed by adoption — it is re-armed like any living adopted worker,
    /// and the stall ladder owns its silence. Mutation caught: killing on "has a
    /// permission row" instead of "has a PENDING one".
    #[test]
    fn adoption_does_not_kill_an_answered_but_quiet_worker() {
        let _spawning = crate::daemon::spawn_probe_guard();
        let (dir, mut ledger, config, mut patrol) = fixture();
        let mut dispatcher = dispatcher_for(dir.path(), &config);
        use std::os::unix::process::CommandExt as _;
        let uuid = "a1150000-0000-4000-8000-00000000dddd";
        let mut cmd = std::process::Command::new("bash");
        cmd.arg0("claude")
            .arg("-c")
            .arg(format!("sleep 30 || true # {uuid}"));
        let mut child = cmd.spawn().unwrap();
        woke_row(
            &mut ledger,
            "t/dev/1",
            "gc-1",
            uuid,
            &dir.path().join("projects/-p/x.jsonl"),
            true,
        );
        ledger
            .append(EventInput {
                kind: EventType::PermissionPending,
                rig: None,
                actor: "campd".into(),
                bead: None,
                data: serde_json::json!({
                    "session": "t/dev/1", "request_id": "cli-2", "tool_name": "Bash",
                }),
            })
            .unwrap();
        // ANSWERED: the request is decided, so the session is no longer blocked.
        ledger
            .append(EventInput {
                kind: EventType::PermissionDecided,
                rig: None,
                actor: "campd".into(),
                bead: None,
                data: serde_json::json!({
                    "session": "t/dev/1", "request_id": "cli-2",
                    "decision": "allow", "decided_by": "operator",
                }),
            })
            .unwrap();

        let summary = adopt(&mut ledger, &mut patrol, &mut dispatcher).unwrap();
        assert!(
            !ledger
                .events_of_type(EventType::SessionCrashed)
                .unwrap()
                .iter()
                .any(|e| e.data["reason"] == "adoption: unanswerable permission request"),
            "an answered worker must not take the unanswerable kill"
        );
        assert_eq!(summary.crashed, 0);
        assert_eq!(summary.rearmed, 1, "{summary:?}");
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn adopt_sweeps_worktrees_by_the_decision_g_table() {
        let _spawning = crate::daemon::spawn_probe_guard();
        let dir = tempfile::tempdir().unwrap();
        // a real git rig so worktree removal works
        let rig = dir.path().join("rig");
        std::fs::create_dir_all(&rig).unwrap();
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.email", "t@t"],
            vec!["config", "user.name", "t"],
            // hermetic: never depend on the host's signing agent
            vec!["config", "commit.gpgsign", "false"],
            vec!["commit", "--allow-empty", "-m", "init"],
        ] {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&rig)
                .args(&args)
                .output()
                .unwrap();
            assert!(out.status.success());
        }
        std::fs::write(
            dir.path().join("camp.toml"),
            format!(
                "[camp]\nname = \"t\"\n\n[[rigs]]\nname = \"gc\"\npath = \"{}\"\nprefix = \"gc\"\n\n\
                 [agent_defaults]\ntools = [\"Read\"]\n",
                rig.display()
            ),
        )
        .unwrap();
        let dev = dir.path().join("agents/dev");
        std::fs::create_dir_all(&dev).unwrap();
        std::fs::write(dev.join("prompt.md"), "Work.\n").unwrap();
        let mut ledger = Ledger::open_with_clock(
            &dir.path().join("camp.db"),
            Box::new(FixedClock::new("2026-07-07T07:00:00Z")),
        )
        .unwrap();
        let config = CampConfig::load(&dir.path().join("camp.toml")).unwrap();
        let patrol_config = camp_core::patrol::PatrolConfig::from_section(&config.patrol).unwrap();
        let mut patrol = PatrolRuntime::new(patrol_config, &config);
        let mut dispatcher = dispatcher_for(dir.path(), &config);
        let camp = crate::campdir::CampDir {
            root: dir.path().to_path_buf(),
        };
        let worktrees = camp.worktrees_path();

        // gc-20: closed pass, interrupted disposition → removed + reaped
        seeded_bead(&mut ledger, "gc-20");
        crate::daemon::spawn::ensure_worktree(&rig, &worktrees, "gc-20", TEST_EXEC_TIMEOUT)
            .unwrap();
        ledger
            .append(EventInput {
                kind: EventType::BeadClosed,
                rig: Some("gc".into()),
                actor: "cli".into(),
                bead: Some("gc-20".into()),
                data: serde_json::json!({"outcome": "pass"}),
            })
            .unwrap();
        // gc-21: closed fail, undisposed → kept with the adopt reason
        seeded_bead(&mut ledger, "gc-21");
        crate::daemon::spawn::ensure_worktree(&rig, &worktrees, "gc-21", TEST_EXEC_TIMEOUT)
            .unwrap();
        ledger
            .append(EventInput {
                kind: EventType::BeadClosed,
                rig: Some("gc".into()),
                actor: "cli".into(),
                bead: Some("gc-21".into()),
                data: serde_json::json!({"outcome": "fail"}),
            })
            .unwrap();
        // gc-22: open → awaiting re-dispatch, untouched, no event
        seeded_bead(&mut ledger, "gc-22");
        crate::daemon::spawn::ensure_worktree(&rig, &worktrees, "gc-22", TEST_EXEC_TIMEOUT)
            .unwrap();
        // gc-999: no such bead → never deleted, reported only
        std::fs::create_dir_all(worktrees.join("gc-999")).unwrap();

        let summary = adopt(&mut ledger, &mut patrol, &mut dispatcher).unwrap();
        assert_eq!(summary.swept, 1, "{summary:?}");
        assert_eq!(summary.kept, 1, "{summary:?}");
        assert!(!worktrees.join("gc-20").exists(), "pass → removed");
        assert!(
            worktrees.join("gc-21").exists(),
            "fail → kept for forensics"
        );
        assert!(worktrees.join("gc-22").exists(), "open → reused later");
        assert!(
            worktrees.join("gc-999").exists(),
            "unattributable → never deleted"
        );
        let events = ledger.events_range(1, None).unwrap();
        let reaped: Vec<_> = events
            .iter()
            .filter(|e| e.kind.as_str() == "bead.worktree.reaped")
            .collect();
        assert_eq!(reaped.len(), 1);
        assert_eq!(reaped[0].bead.as_deref(), Some("gc-20"));
        let kept: Vec<_> = events
            .iter()
            .filter(|e| e.kind.as_str() == "worktree.kept")
            .collect();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].bead.as_deref(), Some("gc-21"));
        assert!(
            kept[0].data["reason"].as_str().unwrap().contains("adopt"),
            "{}",
            kept[0].data
        );

        // exact idempotency for the sweep half: dispositions recorded
        let summary2 = adopt(&mut ledger, &mut patrol, &mut dispatcher).unwrap();
        assert_eq!(summary2, AdoptSummary::default(), "{summary2:?}");
    }

    /// ROUND-1 MINOR 4: a second adopt with a still-live ADOPTED worker in
    /// play is exactly zero — already-tracked sessions are skipped.
    #[test]
    fn adopt_is_idempotent() {
        let _spawning = crate::daemon::spawn_probe_guard();
        let (dir, mut ledger, config, mut patrol) = fixture();
        let mut dispatcher = dispatcher_for(dir.path(), &config);
        use std::os::unix::process::CommandExt as _;
        let live_uuid = "1de40000-0000-4000-8000-00000000cccc";
        let mut live_cmd = std::process::Command::new("bash");
        live_cmd
            .arg0("claude")
            .arg("-c")
            .arg(format!("sleep 30 || true # {live_uuid}"));
        let mut live = live_cmd.spawn().unwrap();
        woke_row(
            &mut ledger,
            "t/dev/1",
            "gc-1",
            live_uuid,
            &dir.path().join("projects/-p/live.jsonl"),
            true,
        );
        let first = adopt(&mut ledger, &mut patrol, &mut dispatcher).unwrap();
        assert_eq!(first.rearmed, 1);
        let events_before = ledger.events_range(1, None).unwrap().len();
        let second = adopt(&mut ledger, &mut patrol, &mut dispatcher).unwrap();
        assert_eq!(second, AdoptSummary::default(), "{second:?}");
        assert_eq!(
            ledger.events_range(1, None).unwrap().len(),
            events_before,
            "a second adopt appends nothing"
        );
        live.kill().unwrap();
        live.wait().unwrap();
    }

    /// ROUND-2 LOW 3: `pgrep -f <uuid>` substring-matches ANY argv — an
    /// operator's `tail -f <transcript-with-uuid>` must never read as the
    /// worker (restart/release would SIGKILL it). The probe accepts only
    /// pids whose leading argv tokens name the configured worker command.
    #[test]
    fn probe_ignores_processes_that_are_not_the_worker_command() {
        let _spawning = crate::daemon::spawn_probe_guard();
        let uuid = "dec0dec0-0000-4000-8000-000000000000";
        // the decoy: uuid in the args, but argv[0] is bash — an operator
        // process, not the worker
        let mut decoy = std::process::Command::new("bash")
            .arg("-c")
            .arg(format!("sleep 30 || true # {uuid}"))
            .spawn()
            .unwrap();
        assert_eq!(
            probe_alive(
                Some(uuid),
                None,
                &HashSet::new(),
                std::path::Path::new("claude"),
                TEST_EXEC_TIMEOUT,
            )
            .unwrap(),
            None,
            "a non-worker process carrying the uuid is INVISIBLE to the probe"
        );
        // the worker shape: argv[0] names the configured command
        use std::os::unix::process::CommandExt as _;
        let mut real = std::process::Command::new("bash");
        real.arg0("claude")
            .arg("-c")
            .arg(format!("sleep 30 || true # {uuid}"));
        let mut worker = real.spawn().unwrap();
        let probed = probe_alive(
            Some(uuid),
            None,
            &HashSet::new(),
            std::path::Path::new("claude"),
            TEST_EXEC_TIMEOUT,
        )
        .unwrap();
        assert_eq!(
            probed,
            Some(i64::from(worker.id())),
            "the worker-shaped process IS the probe's answer"
        );
        decoy.kill().unwrap();
        decoy.wait().unwrap();
        worker.kill().unwrap();
        worker.wait().unwrap();
    }

    /// ROUND-2 LOW 4: kill failures classify by RE-PROBE, never by
    /// locale-dependent stderr text. pid 1 (launchd/init — never ours)
    /// yields EPERM: the old string-match on "No such process" turned
    /// that accepted race-shape into a hard error.
    #[test]
    fn kill_pid_never_classifies_by_stderr_text() {
        let _spawning = crate::daemon::spawn_probe_guard();
        assert!(
            kill_pid(1, TEST_EXEC_TIMEOUT).is_ok(),
            "a failed kill is not an error; the caller's re-probe decides"
        );
    }

    #[test]
    fn release_arms_the_grace_and_kill_released_stops_with_reason() {
        let (dir, mut ledger, config, _p) = fixture();
        // a short grace so the test's timeline is explicit
        let patrol_config = camp_core::patrol::PatrolConfig {
            stall_after: jiff::SignedDuration::from_mins(10),
            restart_budget: 2,
            release_grace: jiff::SignedDuration::from_secs(30),
        };
        let mut patrol = PatrolRuntime::new(patrol_config, &config);
        let mut dispatcher = dispatcher_for(dir.path(), &config);
        let transcript = dir.path().join("projects/-p/sid.jsonl");
        let event = woke_event(&mut ledger, "t/dev/1", "dev", "gc-1", &transcript, "campd");
        track(&mut patrol, &mut ledger, &event, "2026-07-07T07:00:00Z");
        let pid = dispatcher.test_insert_held_cat(dir.path(), "t/dev/1", "gc-1");

        // the bead closes: observe queues the release
        ledger
            .append(EventInput {
                kind: EventType::BeadClaimed,
                rig: Some("gc".into()),
                actor: "cli".into(),
                bead: Some("gc-1".into()),
                data: serde_json::json!({"session": "t/dev/1"}),
            })
            .unwrap();
        let seq = ledger
            .append(EventInput {
                kind: EventType::BeadClosed,
                rig: Some("gc".into()),
                actor: "cli".into(),
                bead: Some("gc-1".into()),
                data: serde_json::json!({"outcome": "pass"}),
            })
            .unwrap();
        let closed = ledger.events_range(seq, Some(seq)).unwrap().remove(0);
        patrol.observe(&closed);
        patrol
            .execute_pending(&mut ledger, &mut dispatcher, ts("2026-07-07T07:20:00Z"))
            .unwrap();

        // the release grace is armed and ignores activity resets
        let fires = patrol.fire_due(ts("2026-07-07T07:20:29Z"));
        assert!(fires.is_empty(), "not before the grace");
        let fires = patrol.fire_due(ts("2026-07-07T07:20:30Z"));
        assert_eq!(fires.len(), 1);
        assert_eq!(fires[0].kind, TimerKind::Release);

        // the grace fires: kill_released -> reap -> stopped with reason
        patrol
            .declare_stalls(&mut ledger, &fires, ts("2026-07-07T07:20:30Z"))
            .unwrap();
        patrol
            .execute_pending(&mut ledger, &mut dispatcher, ts("2026-07-07T07:20:30Z"))
            .unwrap();
        dispatcher.test_child_wait(pid);
        dispatcher.reap(&mut ledger).unwrap();
        let events = ledger.events_range(1, None).unwrap();
        let stopped = events
            .iter()
            .find(|e| e.kind.as_str() == "session.stopped")
            .expect("a released worker stops, never crashes");
        assert!(
            stopped.data["reason"]
                .as_str()
                .unwrap()
                .contains("released"),
            "{}",
            stopped.data
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| e.kind.as_str() == "session.crashed")
                .count(),
            0
        );
    }

    /// compat §6.2 — a released worker's drain-ack reaps it PROMPTLY, at
    /// ack time, without the release_grace timer having fired. Mutation
    /// caught: removing the `WorkerDrainAcked` arm in `observe` (the ack no
    /// longer queues KillReleased; the worker only dies at the full grace).
    #[test]
    fn drain_ack_kills_the_released_worker_promptly_before_the_grace() {
        let (dir, mut ledger, config, _p) = fixture();
        let patrol_config = camp_core::patrol::PatrolConfig {
            stall_after: jiff::SignedDuration::from_mins(10),
            restart_budget: 2,
            release_grace: jiff::SignedDuration::from_secs(30),
        };
        let mut patrol = PatrolRuntime::new(patrol_config, &config);
        let mut dispatcher = dispatcher_for(dir.path(), &config);
        let transcript = dir.path().join("projects/-p/sid.jsonl");
        let event = woke_event(&mut ledger, "t/dev/1", "dev", "gc-1", &transcript, "campd");
        track(&mut patrol, &mut ledger, &event, "2026-07-07T07:00:00Z");
        let pid = dispatcher.test_insert_held_cat(dir.path(), "t/dev/1", "gc-1");

        // close → release (grace armed, worker released but LIVE)
        close_bead(&mut ledger, "gc-1");
        let closed = last_event(&ledger);
        patrol.observe(&closed);
        patrol
            .execute_pending(&mut ledger, &mut dispatcher, ts("2026-07-07T07:20:00Z"))
            .unwrap();

        // drain-ack → prompt KillReleased queued
        drain_ack(&mut ledger, "t/dev/1");
        let dack = last_event(&ledger);
        patrol.observe(&dack);
        assert!(
            patrol.pending.iter().any(|a| matches!(
                a,
                PendingAction::KillReleased { session } if session == "t/dev/1"
            )),
            "drain-ack must queue a prompt KillReleased: {:?}",
            patrol.pending
        );
        // execute ONE SECOND after release — WELL before the 30s grace
        patrol
            .execute_pending(&mut ledger, &mut dispatcher, ts("2026-07-07T07:20:01Z"))
            .unwrap();
        dispatcher.test_child_wait(pid);
        dispatcher.reap(&mut ledger).unwrap();

        let events = ledger.events_range(1, None).unwrap();
        let stopped = events
            .iter()
            .find(|e| e.kind.as_str() == "session.stopped")
            .expect("the ack reaps the worker");
        assert!(
            stopped.data["reason"]
                .as_str()
                .unwrap()
                .contains("released"),
            "{}",
            stopped.data
        );

        // the grace timer still fires later, but the worker is already gone —
        // idempotent no-op, exactly one stop, no crash.
        let fires = patrol.fire_due(ts("2026-07-07T07:20:30Z"));
        patrol
            .declare_stalls(&mut ledger, &fires, ts("2026-07-07T07:20:30Z"))
            .unwrap();
        patrol
            .execute_pending(&mut ledger, &mut dispatcher, ts("2026-07-07T07:20:30Z"))
            .unwrap();
        dispatcher.reap(&mut ledger).unwrap();
        let events = ledger.events_range(1, None).unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|e| e.kind.as_str() == "session.stopped")
                .count(),
            1,
            "exactly one reap — the grace fire is a no-op"
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| e.kind.as_str() == "session.crashed")
                .count(),
            0
        );
    }

    /// compat §6.2 — the race guard: between close and the ack, NOTHING kills
    /// the live-but-released worker early; the ack is what reaps it. (A worker
    /// SIGKILLed mid-handshake is §6.2's race; camp's continuation truncation
    /// keeps the post-close path to a couple of shim calls, so the ack always
    /// beats the grace.)
    #[test]
    fn a_slow_drain_that_acks_before_the_grace_is_not_killed_early() {
        let (dir, mut ledger, config, _p) = fixture();
        let patrol_config = camp_core::patrol::PatrolConfig {
            stall_after: jiff::SignedDuration::from_mins(10),
            restart_budget: 2,
            release_grace: jiff::SignedDuration::from_secs(30),
        };
        let mut patrol = PatrolRuntime::new(patrol_config, &config);
        let mut dispatcher = dispatcher_for(dir.path(), &config);
        let transcript = dir.path().join("projects/-p/sid.jsonl");
        let event = woke_event(&mut ledger, "t/dev/1", "dev", "gc-1", &transcript, "campd");
        track(&mut patrol, &mut ledger, &event, "2026-07-07T07:00:00Z");
        let pid = dispatcher.test_insert_held_cat(dir.path(), "t/dev/1", "gc-1");

        close_bead(&mut ledger, "gc-1");
        let closed = last_event(&ledger);
        patrol.observe(&closed);
        patrol
            .execute_pending(&mut ledger, &mut dispatcher, ts("2026-07-07T07:20:00Z"))
            .unwrap();

        // 15s later — still inside the 30s grace — nothing has fired, so the
        // worker was NOT killed early.
        assert!(
            patrol.fire_due(ts("2026-07-07T07:20:15Z")).is_empty(),
            "no timer fires before the grace"
        );
        dispatcher.reap(&mut ledger).unwrap();
        assert_eq!(
            ledger
                .events_range(1, None)
                .unwrap()
                .iter()
                .filter(|e| e.kind.as_str() == "session.stopped")
                .count(),
            0,
            "the released worker is still live before its ack"
        );

        // now it acks (at t=15s < grace) → reaped by the ack path.
        drain_ack(&mut ledger, "t/dev/1");
        let dack = last_event(&ledger);
        patrol.observe(&dack);
        patrol
            .execute_pending(&mut ledger, &mut dispatcher, ts("2026-07-07T07:20:15Z"))
            .unwrap();
        dispatcher.test_child_wait(pid);
        dispatcher.reap(&mut ledger).unwrap();
        assert!(
            ledger
                .events_range(1, None)
                .unwrap()
                .iter()
                .any(|e| e.kind.as_str() == "session.stopped"),
            "the ack reaps the slow-draining worker"
        );
    }

    /// compat §6.2 — the backstop: a worker that NEVER acks (crashed mid-drain,
    /// or a native non-gc worker) is still reaped by the release_grace timer.
    /// Mutation caught: removing the bead-close Release arm (a crashed-mid-drain
    /// worker would then leak forever).
    #[test]
    fn a_worker_that_never_acks_is_killed_by_the_grace_backstop() {
        let (dir, mut ledger, config, _p) = fixture();
        let patrol_config = camp_core::patrol::PatrolConfig {
            stall_after: jiff::SignedDuration::from_mins(10),
            restart_budget: 2,
            release_grace: jiff::SignedDuration::from_secs(30),
        };
        let mut patrol = PatrolRuntime::new(patrol_config, &config);
        let mut dispatcher = dispatcher_for(dir.path(), &config);
        let transcript = dir.path().join("projects/-p/sid.jsonl");
        let event = woke_event(&mut ledger, "t/dev/1", "dev", "gc-1", &transcript, "campd");
        track(&mut patrol, &mut ledger, &event, "2026-07-07T07:00:00Z");
        let pid = dispatcher.test_insert_held_cat(dir.path(), "t/dev/1", "gc-1");

        close_bead(&mut ledger, "gc-1");
        let closed = last_event(&ledger);
        patrol.observe(&closed);
        patrol
            .execute_pending(&mut ledger, &mut dispatcher, ts("2026-07-07T07:20:00Z"))
            .unwrap();

        // no ack ever arrives; the grace fires and reaps.
        let fires = patrol.fire_due(ts("2026-07-07T07:20:30Z"));
        assert_eq!(fires.len(), 1);
        assert_eq!(fires[0].kind, TimerKind::Release);
        patrol
            .declare_stalls(&mut ledger, &fires, ts("2026-07-07T07:20:30Z"))
            .unwrap();
        patrol
            .execute_pending(&mut ledger, &mut dispatcher, ts("2026-07-07T07:20:30Z"))
            .unwrap();
        dispatcher.test_child_wait(pid);
        dispatcher.reap(&mut ledger).unwrap();
        assert!(
            ledger
                .events_range(1, None)
                .unwrap()
                .iter()
                .any(|e| e.kind.as_str() == "session.stopped"),
            "the grace backstop reaps a worker that never acks"
        );
    }

    /// compat §6.2 (NB2) — at the grace boundary, the ack and the grace fire
    /// land in the same wake; the worker is reaped EXACTLY ONCE (no double
    /// kill_released, no panic). Pins the "post-close drain ≪ grace" claim by
    /// forcing the boundary with a short grace.
    #[test]
    fn a_drain_ack_at_the_grace_boundary_still_reaps_via_the_ack_not_a_double_kill() {
        let (dir, mut ledger, config, _p) = fixture();
        let patrol_config = camp_core::patrol::PatrolConfig {
            stall_after: jiff::SignedDuration::from_mins(10),
            restart_budget: 2,
            release_grace: jiff::SignedDuration::from_secs(1), // SHORT, to force the boundary
        };
        let mut patrol = PatrolRuntime::new(patrol_config, &config);
        let mut dispatcher = dispatcher_for(dir.path(), &config);
        let transcript = dir.path().join("projects/-p/sid.jsonl");
        let event = woke_event(&mut ledger, "t/dev/1", "dev", "gc-1", &transcript, "campd");
        track(&mut patrol, &mut ledger, &event, "2026-07-07T07:00:00Z");
        let pid = dispatcher.test_insert_held_cat(dir.path(), "t/dev/1", "gc-1");

        close_bead(&mut ledger, "gc-1");
        let closed = last_event(&ledger);
        patrol.observe(&closed);
        patrol
            .execute_pending(&mut ledger, &mut dispatcher, ts("2026-07-07T07:20:00Z"))
            .unwrap();

        // the ack AND the 1s-grace fire both land at t=+1s. Process the ack's
        // KillReleased AND the grace's KillReleased in one execute pass.
        drain_ack(&mut ledger, "t/dev/1");
        let dack = last_event(&ledger);
        patrol.observe(&dack);
        let fires = patrol.fire_due(ts("2026-07-07T07:20:01Z"));
        patrol
            .declare_stalls(&mut ledger, &fires, ts("2026-07-07T07:20:01Z"))
            .unwrap();
        // both KillReleased actions are now queued; executing them must not
        // panic and must reap the worker exactly once.
        patrol
            .execute_pending(&mut ledger, &mut dispatcher, ts("2026-07-07T07:20:01Z"))
            .unwrap();
        dispatcher.test_child_wait(pid);
        dispatcher.reap(&mut ledger).unwrap();
        assert_eq!(
            ledger
                .events_range(1, None)
                .unwrap()
                .iter()
                .filter(|e| e.kind.as_str() == "session.stopped")
                .count(),
            1,
            "the boundary reaps exactly once — idempotent, no double kill"
        );
    }

    /// #81: after apply_config swaps in a config whose pack ships an agent the
    /// BIRTH config could not see, patrol resolves that agent — no
    /// patrol.degraded, and the agent's own stall_after governs the armed
    /// timer (proving resolution ran against the reloaded config, not the
    /// birth one).
    #[test]
    fn apply_config_lets_patrol_resolve_a_reloaded_pack_agent() {
        let dir = tempfile::tempdir().unwrap();
        // A pack shipping agent "sentry" with a DISTINCT stall_after override.
        // A local-path import is layered IN PLACE (§5) — no materialization
        // step, so the pack is simply a directory beside camp.toml.
        let sentry = dir.path().join("sentrypack/agents/sentry");
        std::fs::create_dir_all(&sentry).unwrap();
        std::fs::write(
            sentry.join("agent.toml"),
            "isolation = \"none\"\nstall_after = \"700ms\"\n",
        )
        .unwrap();
        std::fs::write(sentry.join("prompt.md"), "Work.\n").unwrap();

        // Birth config: NO imports, a distinct camp-default stall_after of 5s.
        let birth_toml = "[camp]\nname = \"t\"\n\n[patrol]\nstall_after = \"5s\"\n\n\
                          [agent_defaults]\ntools = [\"Read\"]\n";
        std::fs::write(dir.path().join("camp.toml"), birth_toml).unwrap();
        let birth = CampConfig::load(&dir.path().join("camp.toml")).unwrap();
        let patrol_config = camp_core::patrol::PatrolConfig::from_section(&birth.patrol).unwrap();
        let mut patrol = PatrolRuntime::new(patrol_config, &birth);

        // Reloaded config: binds the pack (so "pack.sentry" becomes resolvable).
        let reloaded_toml = "[camp]\nname = \"t\"\n\n[patrol]\nstall_after = \"5s\"\n\n\
                             [agent_defaults]\ntools = [\"Read\"]\n\n\
                             [imports.pack]\nsource = \"sentrypack\"\n";
        std::fs::write(dir.path().join("camp.toml"), reloaded_toml).unwrap();
        let reloaded = CampConfig::load(&dir.path().join("camp.toml")).unwrap();
        assert!(
            pack::resolve_agent(&birth, "pack.sentry").is_err(),
            "the BIRTH config cannot see the agent — that is what makes the reload load-bearing"
        );
        patrol.apply_config(reloaded).unwrap();

        // Drive a campd-spawned (Owned::Child) worker for the pack agent through
        // observe -> apply_tracking, exactly as the settle path does. Append the
        // session.woke through the ledger and read it back so the Event has the
        // exact shape the fold produces (mirrors event_loop.rs's test
        // a_due_stall_declares_and_the_settle_executes_the_action).
        let mut ledger = Ledger::open(&dir.path().join("camp.db")).unwrap();
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
                kind: EventType::SessionWoke,
                rig: Some("gc".into()),
                actor: "campd".into(),
                bead: Some("gc-1".into()),
                data: serde_json::json!({
                    "name": "t/sentry/1",
                    "agent": "pack.sentry",
                    "transcript_path": dir.path().join("projects/-p/sid.jsonl"),
                    "bead": "gc-1",
                }),
            })
            .unwrap();
        let events = ledger.events_range(1, None).unwrap();
        let woke = events
            .iter()
            .find(|e| e.kind == EventType::SessionWoke)
            .unwrap();
        patrol.observe(woke);
        let now = jiff::Timestamp::now();
        patrol.apply_tracking(&mut ledger, now).unwrap();

        // No unknown-agent degradation: resolution ran against the reloaded config.
        let degraded = ledger.events_of_type(EventType::PatrolDegraded).unwrap();
        assert!(
            degraded.is_empty(),
            "patrol must resolve the reloaded pack agent, got: {degraded:?}"
        );

        // And the arm used the AGENT's 700ms override, not the 5s camp default:
        // fire it just past 700ms and read the declared threshold.
        let later = now
            .checked_add(jiff::SignedDuration::from_millis(750))
            .unwrap();
        let fires = patrol.fire_due(later);
        assert_eq!(fires.len(), 1, "the 700ms agent threshold fired by 750ms");
        patrol.declare_stalls(&mut ledger, &fires, later).unwrap();
        let stalled = ledger.events_of_type(EventType::AgentStalled).unwrap();
        assert_eq!(
            stalled[0].data["threshold"], "700ms",
            "patrol armed at the reloaded agent's stall_after, not the camp default"
        );
    }

    /// #81 structural guard: apply_config must swap EVERY config-derived
    /// cached field. The reload below differs from the birth config in every
    /// derived surface — `[patrol]` (→ `self.config`: stall_after,
    /// release_grace, restart_budget), the pack list (→ `self.camp_config`:
    /// agent resolution), and the ladder's restart-budget ceiling — and each
    /// is asserted changed. Whole-struct equality on `config` / `camp_config`
    /// means a future field added to PatrolConfig or CampConfig but forgotten
    /// in apply_config turns this test red instead of quietly reviving the
    /// #81 defect class one field over; the ladder ceiling (private to
    /// camp-core) is pinned by on_fire behavior.
    #[test]
    fn apply_config_swaps_every_config_derived_field() {
        let dir = tempfile::tempdir().unwrap();
        // A local-path pack, layered in place (§5) — a directory agent.
        let sentry = dir.path().join("sentrypack/agents/sentry");
        std::fs::create_dir_all(&sentry).unwrap();
        std::fs::write(sentry.join("agent.toml"), "isolation = \"none\"\n").unwrap();
        std::fs::write(sentry.join("prompt.md"), "Work.\n").unwrap();

        // Birth: no imports; every [patrol] key explicit; restart_budget 0.
        let birth_toml = "[camp]\nname = \"t\"\n\n[patrol]\nstall_after = \"5s\"\n\
                          release_grace = \"30s\"\nrestart_budget = 0\n\n\
                          [agent_defaults]\ntools = [\"Read\"]\n";
        std::fs::write(dir.path().join("camp.toml"), birth_toml).unwrap();
        let birth = CampConfig::load(&dir.path().join("camp.toml")).unwrap();
        let birth_patrol = camp_core::patrol::PatrolConfig::from_section(&birth.patrol).unwrap();
        let mut patrol = PatrolRuntime::new(birth_patrol.clone(), &birth);

        // With the birth budget of 0, "gc-g" would exhaust on its second fire.
        assert_eq!(patrol.ladder.on_fire("gc-g"), LadderAction::Nudge);

        // Reload: EVERY derived field differs — the import bound, all three
        // [patrol] keys changed.
        let reloaded_toml = "[camp]\nname = \"t\"\n\n[patrol]\nstall_after = \"9s\"\n\
                             release_grace = \"77s\"\nrestart_budget = 3\n\n\
                             [agent_defaults]\ntools = [\"Read\"]\n\n\
                             [imports.pack]\nsource = \"sentrypack\"\n";
        std::fs::write(dir.path().join("camp.toml"), reloaded_toml).unwrap();
        let reloaded = CampConfig::load(&dir.path().join("camp.toml")).unwrap();
        let reloaded_patrol =
            camp_core::patrol::PatrolConfig::from_section(&reloaded.patrol).unwrap();
        // Vacuity guard: the fixtures must actually differ everywhere, or the
        // equality assertions below prove nothing.
        assert_ne!(birth_patrol, reloaded_patrol);
        assert_ne!(birth_patrol.stall_after, reloaded_patrol.stall_after);
        assert_ne!(birth_patrol.release_grace, reloaded_patrol.release_grace);
        assert_ne!(birth_patrol.restart_budget, reloaded_patrol.restart_budget);
        assert_ne!(birth, reloaded);
        assert!(pack::resolve_agent(&birth, "pack.sentry").is_err());

        patrol.apply_config(reloaded.clone()).unwrap();

        // self.config: the whole derived PatrolConfig followed the reload.
        assert_eq!(
            patrol.config, reloaded_patrol,
            "apply_config must swap the entire derived PatrolConfig"
        );
        // self.camp_config: the whole CampConfig followed the reload, and the
        // change is agent-resolution-visible.
        assert_eq!(
            patrol.camp_config, reloaded,
            "apply_config must swap the entire cached CampConfig"
        );
        assert!(
            pack::resolve_agent(&patrol.camp_config, "pack.sentry").is_ok(),
            "the reloaded pack agent must resolve against patrol's config"
        );
        // The ladder ceiling: under the birth budget (0) this second fire
        // would be Exhausted; the reloaded budget (3) makes it Restart —
        // and the bead's history was preserved, not reset.
        assert_eq!(
            patrol.ladder.on_fire("gc-g"),
            LadderAction::Restart,
            "the ladder ceiling must follow the reloaded restart_budget"
        );
    }
}

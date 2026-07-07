//! campd's order machinery (spec §9): the compiled-order runtime (cron
//! heap + event orders + hot reload) and the processor that puts order
//! evaluation on the same post-commit processing path as readiness —
//! zero standing cost, exactly-once via the cursor transaction.
//!
//! The settle loop is the fire pipeline's engine (plan Decision D): every
//! fire is an `order.fired` event; processing one queues its cook;
//! `settle` runs catch-up and cook execution to a fixpoint. Convergence,
//! not polling — each iteration consumes ledger progress.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use camp_core::clock::Clock;
use camp_core::config::CampConfig;
use camp_core::error::CoreError;
use camp_core::event::{Event, EventInput, EventType};
use camp_core::ledger::Ledger;
use camp_core::orders::cron::{CatchUp, CronHeap, Fire};
use camp_core::orders::parse::compile_orders;
use camp_core::orders::{
    FireCause, Order, PendingCook, Trigger, completion_input, event_trigger_matches, execute_fire,
    fired_input, pending_cook_from_fired,
};
use jiff::Timestamp;
use jiff::tz::TimeZone;

use super::cursor::{self, EventProcessor, ReadinessProcessor};

/// campd's compiled-order state: the config text last applied, the
/// compiled orders, the armed cron heap, and the cooks queued by the
/// processor for the settle loop.
pub struct OrdersRuntime {
    camp_root: PathBuf,
    tz: TimeZone,
    raw: String,
    config: CampConfig,
    orders: Vec<Order>,
    heap: CronHeap,
    pending_cooks: Vec<PendingCook>,
}

impl OrdersRuntime {
    /// Load camp.toml, compile its orders, and arm the heap at `now`. A
    /// config that does not parse, or a cron order that never fires, is a
    /// hard error: campd refuses to run with broken declared automation
    /// (fail fast).
    pub fn build(camp_root: &Path, now: Timestamp, tz: TimeZone) -> Result<Self> {
        let config_path = camp_root.join("camp.toml");
        let raw = std::fs::read_to_string(&config_path)
            .with_context(|| format!("reading {}", config_path.display()))?;
        let (config, orders, heap) = compile_and_arm(&raw, now, &tz)?;
        Ok(OrdersRuntime {
            camp_root: camp_root.to_path_buf(),
            tz,
            raw,
            config,
            orders,
            heap,
            pending_cooks: Vec::new(),
        })
    }

    /// The earliest armed deadline as a poll timeout (spec §9, invariant 1):
    /// `None` = no timers = infinite wait; a due-or-past deadline = ZERO
    /// (poll returns immediately and `fire_due` fires); otherwise the time
    /// to the deadline rounded UP 1 ms so the wake lands at-or-after it —
    /// never a hot spin just before.
    pub fn poll_timeout(&self, now: Timestamp) -> Option<Duration> {
        let deadline = self.heap.next_deadline()?;
        let until = deadline.duration_since(now);
        if until.is_negative() || until.is_zero() {
            return Some(Duration::ZERO);
        }
        let until = Duration::try_from(until).unwrap_or(Duration::MAX);
        Some(until.saturating_add(Duration::from_millis(1)))
    }

    pub fn order(&self, name: &str) -> Option<&Order> {
        self.orders.iter().find(|o| o.name == name)
    }

    pub fn fire_due(&mut self, now: Timestamp) -> Vec<Fire> {
        self.heap.fire_due(now)
    }

    pub fn recompute(&mut self, now: Timestamp, last_seen: Timestamp) -> Vec<CatchUp> {
        self.heap.recompute(now, last_seen)
    }

    pub fn queue_cook(&mut self, cook: PendingCook) {
        self.pending_cooks.push(cook);
    }

    pub fn take_pending_cooks(&mut self) -> Vec<PendingCook> {
        std::mem::take(&mut self.pending_cooks)
    }

    pub fn pending_cook_count(&self) -> usize {
        self.pending_cooks.len()
    }

    /// Hot reload (spec §13.4, plan Decision H). Reads camp.toml and
    /// compares bytes against the last applied text: identical → `None`
    /// (editor double-events cost nothing — no debounce timers). A real
    /// change returns the `config.changed` event input for the caller to
    /// append: applied (state swapped, heap re-armed at `now`) or rejected
    /// (old config retained; a running daemon must survive a torn
    /// mid-editor write, and the error is durable in the ledger, not just
    /// a log line).
    pub fn reload_if_changed(&mut self, now: Timestamp) -> Result<Option<EventInput>> {
        let config_path = self.camp_root.join("camp.toml");
        let raw = std::fs::read_to_string(&config_path)
            .with_context(|| format!("reading {}", config_path.display()))?;
        if raw == self.raw {
            return Ok(None);
        }
        let data = match compile_and_arm(&raw, now, &self.tz) {
            Ok((config, orders, heap)) => {
                let count = orders.len();
                self.raw = raw;
                self.config = config;
                self.orders = orders;
                self.heap = heap;
                serde_json::json!({
                    "path": "camp.toml", "applied": true, "orders": count,
                })
            }
            Err(e) => {
                // Remember the rejected text so an unchanged bad file does
                // not re-event on every watch wake; the last APPLIED config
                // keeps running.
                self.raw = raw;
                serde_json::json!({
                    "path": "camp.toml", "applied": false, "error": format!("{e:#}"),
                })
            }
        };
        Ok(Some(EventInput {
            kind: EventType::ConfigChanged,
            rig: None,
            actor: "campd".into(),
            bead: None,
            data,
        }))
    }
}

fn compile_and_arm(
    raw: &str,
    now: Timestamp,
    tz: &TimeZone,
) -> Result<(CampConfig, Vec<Order>, CronHeap)> {
    let config = CampConfig::parse(raw).context("parsing camp.toml")?;
    let orders = compile_orders(&config).context("compiling [[order]] tables")?;
    let mut heap = CronHeap::new(tz.clone());
    for order in &orders {
        if matches!(order.trigger, Trigger::Cron { .. }) {
            heap.arm(order.clone(), now)?;
        }
    }
    Ok((config, orders, heap))
}

/// The downtime catch-up anchor (spec §9; PR #13 review MEDIUM 2): the
/// `ts` of the event at campd's CURSOR position — the last instant campd
/// demonstrably observed the world. Anchoring on the ledger's last event
/// of any actor would let a daemon-less CLI write mask a missed cron fire
/// that spec §9 guarantees fires once on wake. campd's own fires advance
/// the cursor when processed, so nothing refires across a restart. A
/// never-advanced cursor (fresh camp) anchors at `now`: nothing was ever
/// scheduled, nothing to catch up. Read BEFORE the startup settle — settle
/// advances the cursor.
pub fn catch_up_anchor(ledger: &Ledger, now: Timestamp) -> Result<Timestamp, CoreError> {
    let cursor = ledger.cursor(cursor::CAMPD_CURSOR)?;
    if cursor == 0 {
        return Ok(now);
    }
    let event = ledger
        .events_range(cursor, Some(cursor))?
        .into_iter()
        .next()
        .ok_or_else(|| {
            CoreError::Corrupt(format!("campd cursor points at missing event seq {cursor}"))
        })?;
    event.ts.parse().map_err(|e| {
        CoreError::Corrupt(format!(
            "event seq {cursor} has unparseable ts {:?}: {e}",
            event.ts
        ))
    })
}

/// The campd processor: readiness (Phase 7) plus orders (Phase 10), one
/// pass per committed event, inside the cursor transaction.
pub struct CampdProcessor<'a> {
    pub readiness: &'a mut ReadinessProcessor,
    pub runtime: &'a mut OrdersRuntime,
    pub clock: &'a dyn Clock,
}

impl EventProcessor for CampdProcessor<'_> {
    fn process(&mut self, conn: &rusqlite::Connection, event: &Event) -> Result<(), CoreError> {
        // (1) readiness bookkeeping — untouched Phase 7 behavior
        self.readiness.process(conn, event)?;
        // (2) a fire declaration → queue its cook for the settle loop
        if let Some(cook) = pending_cook_from_fired(event)? {
            self.runtime.queue_cook(cook);
        }
        // (3) an order-cooked run root closing → completion, atomic with
        //     the cursor advance (plan Decision C)
        if let Some(input) = completion_input(conn, event)? {
            Ledger::append_on(conn, &self.clock.now_utc(), input)?;
        }
        // (4) event-triggered orders: the match's order.fired commits with
        //     this event's cursor advance; the SAME catch_up call drains it
        //     (process_past_cursor pages until empty) and queues its cook.
        let mut fired = Vec::new();
        for order in &self.runtime.orders {
            if event_trigger_matches(conn, order, event)? {
                fired.push(order.name.clone());
            }
        }
        for name in fired {
            Ledger::append_on(
                conn,
                &self.clock.now_utc(),
                fired_input(
                    &name,
                    &FireCause::Event {
                        cause_seq: event.seq,
                    },
                ),
            )?;
        }
        Ok(())
    }
}

/// Catch up and cook to a fixpoint: `catch_up` drains every committed
/// event (queueing cooks, firing event orders, appending completions);
/// queued cooks then execute; their events demand another round. Bounded
/// by ledger progress — convergence, not polling.
///
/// Order-level cook failures are already evented by `execute_fire`
/// (`Ok(None)`) and never stop the loop (plan Decision K); only
/// infrastructure errors surface.
pub fn settle(
    ledger: &mut Ledger,
    readiness: &mut ReadinessProcessor,
    runtime: &mut OrdersRuntime,
    clock: &dyn Clock,
) -> Result<(), CoreError> {
    loop {
        {
            let mut processor = CampdProcessor {
                readiness,
                runtime,
                clock,
            };
            cursor::catch_up(ledger, &mut processor)?;
        }
        // Phase 8's dispatcher consumes this; drained so the bookkeeping
        // stays bounded in a long-lived daemon (Phase 7 precedent).
        let _newly_ready = readiness.take_pending();
        let cooks = runtime.take_pending_cooks();
        if cooks.is_empty() {
            return Ok(());
        }
        for (i, cook) in cooks.iter().enumerate() {
            let outcome = match runtime.order(&cook.order) {
                Some(order) => {
                    let order = order.clone();
                    // Ok(None) = deduped or evented order-level failure —
                    // both fine; the loop continues.
                    execute_fire(
                        ledger,
                        &runtime.config,
                        &runtime.camp_root,
                        &order,
                        cook.fired_seq,
                    )
                    .map(|_| ())
                }
                None => {
                    // The order vanished (reload) between fire and cook:
                    // evented, never silent.
                    ledger
                        .append(EventInput {
                            kind: EventType::OrderFailed,
                            rig: None,
                            actor: "campd".into(),
                            bead: None,
                            data: serde_json::json!({
                                "order": cook.order,
                                "fired_seq": cook.fired_seq,
                                "error": "order no longer configured at cook time",
                            }),
                        })
                        .map(|_| ())
                }
            };
            if let Err(error) = outcome {
                // Infrastructure error (PR #13 review MEDIUM 3): the cursor
                // is already past these fires' order.fired events, so a
                // dropped cook would never run again until a restart's
                // reconciliation. Requeue the failing cook and every
                // unexecuted one, then surface the error — the next settle
                // (poke, timer, reload) retries; dedupe keeps replays safe.
                for survivor in &cooks[i..] {
                    runtime.queue_cook(survivor.clone());
                }
                return Err(error);
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use camp_core::clock::FixedClock;
    use camp_core::event::{EventInput, EventType};
    use camp_core::ledger::Ledger;
    use camp_core::orders::{FireCause, fired_input};
    use jiff::Timestamp;
    use jiff::tz::TimeZone;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    const BASE_TOML: &str =
        "[camp]\nname=\"d\"\n\n[[rigs]]\nname=\"gc\"\npath=\"/p\"\nprefix=\"gc\"\n";

    /// A camp root on disk: camp.toml (+ orders), formulas/one-step.toml,
    /// and a ledger.
    fn fixture(orders_toml: &str) -> (tempfile::TempDir, Ledger) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("camp.toml"),
            format!("{BASE_TOML}{orders_toml}"),
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("formulas")).unwrap();
        std::fs::write(
            dir.path().join("formulas/one-step.toml"),
            "formula = \"one-step\"\n\n[[steps]]\nid = \"s1\"\ntitle = \"one step\"\n",
        )
        .unwrap();
        let ledger = Ledger::open(&dir.path().join("camp.db")).unwrap();
        (dir, ledger)
    }

    fn runtime(dir: &tempfile::TempDir, now: &str) -> OrdersRuntime {
        OrdersRuntime::build(dir.path(), ts(now), TimeZone::UTC).unwrap()
    }

    fn clock() -> FixedClock {
        FixedClock::new("2026-07-06T07:00:00Z")
    }

    fn settle_all(ledger: &mut Ledger, runtime: &mut OrdersRuntime) {
        let mut readiness = ReadinessProcessor::default();
        settle(ledger, &mut readiness, runtime, &clock()).unwrap();
    }

    fn types(ledger: &Ledger) -> Vec<String> {
        ledger
            .events_range(1, None)
            .unwrap()
            .iter()
            .map(|e| e.kind.as_str().to_owned())
            .collect()
    }

    #[test]
    fn build_rejects_bad_config_and_never_firing_cron() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("camp.toml"), "not toml [[[").unwrap();
        assert!(
            OrdersRuntime::build(dir.path(), ts("2026-07-06T07:00:00Z"), TimeZone::UTC).is_err()
        );

        let (dir, _ledger) =
            fixture("[[order]]\nname=\"dead\"\non=\"cron:0 0 30 2 *\"\nformula=\"one-step\"\n");
        let Err(err) = OrdersRuntime::build(dir.path(), ts("2026-07-06T07:00:00Z"), TimeZone::UTC)
        else {
            panic!("a never-firing cron order must be rejected at build")
        };
        assert!(err.to_string().contains("dead"), "{err}");
    }

    #[test]
    fn poll_timeout_is_none_when_idle_and_deadline_based_when_armed() {
        let (dir, _ledger) = fixture("");
        let rt = runtime(&dir, "2026-07-06T07:20:00Z");
        assert_eq!(rt.poll_timeout(ts("2026-07-06T07:20:00Z")), None);

        let (dir, _ledger) =
            fixture("[[order]]\nname=\"h\"\non=\"cron:0 8 * * *\"\nformula=\"one-step\"\n");
        let rt = runtime(&dir, "2026-07-06T07:20:00Z");
        let timeout = rt.poll_timeout(ts("2026-07-06T07:59:59Z")).unwrap();
        // 1 s to the deadline, rounded up by 1 ms
        assert!(timeout >= std::time::Duration::from_secs(1), "{timeout:?}");
        assert!(
            timeout <= std::time::Duration::from_millis(1500),
            "{timeout:?}"
        );
    }

    #[test]
    fn poll_timeout_rounds_up_and_never_spins() {
        let (dir, _ledger) =
            fixture("[[order]]\nname=\"h\"\non=\"cron:0 8 * * *\"\nformula=\"one-step\"\n");
        let rt = runtime(&dir, "2026-07-06T07:20:00Z");
        // just before the deadline: at least the 1 ms round-up remains
        let timeout = rt.poll_timeout(ts("2026-07-06T07:59:59.999999Z")).unwrap();
        assert!(
            timeout >= std::time::Duration::from_millis(1),
            "{timeout:?}"
        );
        // past the deadline: zero — poll returns immediately, fire_due fires
        assert_eq!(
            rt.poll_timeout(ts("2026-07-06T08:00:01Z")),
            Some(std::time::Duration::ZERO)
        );
    }

    #[test]
    fn settle_cooks_a_manual_fire_and_completes_on_root_close() {
        let (dir, mut ledger) =
            fixture("[[order]]\nname=\"one-shot\"\non=\"cron:0 0 1 1 *\"\nformula=\"one-step\"\n");
        let mut rt = runtime(&dir, "2026-07-06T07:20:00Z");
        let fired = ledger
            .append(fired_input("one-shot", &FireCause::Manual))
            .unwrap();
        settle_all(&mut ledger, &mut rt);
        // fired → cooked: run.cooked with the order actor, beads exist
        let cooked = ledger.events_of_type(EventType::RunCooked).unwrap();
        assert_eq!(cooked.len(), 1);
        assert_eq!(cooked[0].actor, format!("order:one-shot:{fired}"));
        let root = cooked[0].data["root"].as_str().unwrap().to_owned();
        let step = cooked[0].data["steps"]["s1"].as_str().unwrap().to_owned();
        // the fake-agent contract via the ledger: claim+close step, close root
        for (kind, bead, data) in [
            (
                EventType::BeadClaimed,
                &step,
                serde_json::json!({"session":"s"}),
            ),
            (
                EventType::BeadClosed,
                &step,
                serde_json::json!({"outcome":"pass"}),
            ),
            (
                EventType::BeadClosed,
                &root,
                serde_json::json!({"outcome":"pass"}),
            ),
        ] {
            ledger
                .append(EventInput {
                    kind,
                    rig: Some("gc".into()),
                    actor: "session:s".into(),
                    bead: Some(bead.clone()),
                    data,
                })
                .unwrap();
        }
        settle_all(&mut ledger, &mut rt);
        let completed = ledger.events_of_type(EventType::OrderCompleted).unwrap();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].data["order"], "one-shot");
        assert_eq!(completed[0].data["fired_seq"], fired);
        assert_eq!(completed[0].data["root_bead"], root);
        assert_eq!(completed[0].data["outcome"], "pass");
        // idempotent: another settle appends nothing
        let before = types(&ledger).len();
        settle_all(&mut ledger, &mut rt);
        assert_eq!(types(&ledger).len(), before);
    }

    #[test]
    fn settle_fires_event_orders_on_matching_close_only() {
        let (dir, mut ledger) = fixture(
            "[[order]]\nname=\"ci-red\"\non=\"event:bead.closed[label=ci-red]\"\nformula=\"one-step\"\n",
        );
        let mut rt = runtime(&dir, "2026-07-06T07:20:00Z");
        // an unlabeled bead closing: no fire
        ledger
            .append(EventInput {
                kind: EventType::BeadCreated,
                rig: Some("gc".into()),
                actor: "cli".into(),
                bead: Some("gc-1".into()),
                data: serde_json::json!({"title":"plain"}),
            })
            .unwrap();
        ledger
            .append(EventInput {
                kind: EventType::BeadClosed,
                rig: Some("gc".into()),
                actor: "cli".into(),
                bead: Some("gc-1".into()),
                data: serde_json::json!({"outcome":"pass"}),
            })
            .unwrap();
        settle_all(&mut ledger, &mut rt);
        assert!(
            ledger
                .events_of_type(EventType::OrderFired)
                .unwrap()
                .is_empty()
        );
        // a labeled bead closing: fire + cook in ONE settle call (fixpoint)
        ledger
            .append(EventInput {
                kind: EventType::BeadCreated,
                rig: Some("gc".into()),
                actor: "cli".into(),
                bead: Some("gc-2".into()),
                data: serde_json::json!({"title":"red","labels":["ci-red"]}),
            })
            .unwrap();
        let close_seq = ledger
            .append(EventInput {
                kind: EventType::BeadClosed,
                rig: Some("gc".into()),
                actor: "cli".into(),
                bead: Some("gc-2".into()),
                data: serde_json::json!({"outcome":"pass"}),
            })
            .unwrap();
        settle_all(&mut ledger, &mut rt);
        let fired = ledger.events_of_type(EventType::OrderFired).unwrap();
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].data["trigger"], "event");
        assert_eq!(fired[0].data["cause_seq"], close_seq);
        assert_eq!(
            ledger.events_of_type(EventType::RunCooked).unwrap().len(),
            1,
            "the fire's cook happens in the same settle call"
        );
    }

    #[test]
    fn settle_survives_a_broken_order_and_events_the_failure() {
        let (dir, mut ledger) = fixture(
            "[[order]]\nname=\"broken\"\non=\"cron:0 0 1 1 *\"\nformula=\"no-such-formula\"\n",
        );
        let mut rt = runtime(&dir, "2026-07-06T07:20:00Z");
        ledger
            .append(fired_input("broken", &FireCause::Manual))
            .unwrap();
        settle_all(&mut ledger, &mut rt); // must not error
        let failed = ledger.events_of_type(EventType::OrderFailed).unwrap();
        assert_eq!(failed.len(), 1);
        assert!(
            failed[0].data["error"]
                .as_str()
                .unwrap()
                .contains("no-such-formula")
        );
    }

    #[test]
    fn settle_events_a_fire_whose_order_is_gone() {
        let (dir, mut ledger) = fixture("");
        let mut rt = runtime(&dir, "2026-07-06T07:20:00Z");
        ledger
            .append(fired_input("ghost", &FireCause::Manual))
            .unwrap();
        settle_all(&mut ledger, &mut rt);
        let failed = ledger.events_of_type(EventType::OrderFailed).unwrap();
        assert_eq!(failed.len(), 1);
        assert!(
            failed[0].data["error"]
                .as_str()
                .unwrap()
                .contains("no longer configured"),
            "{}",
            failed[0].data
        );
    }

    #[test]
    fn reload_swaps_config_and_reports_rejects() {
        let (dir, _ledger) = fixture("");
        let mut rt = runtime(&dir, "2026-07-06T07:20:00Z");
        let now = ts("2026-07-06T07:21:00Z");
        // unchanged content → no event
        assert!(rt.reload_if_changed(now).unwrap().is_none());
        // a new order appears → applied
        std::fs::write(
            dir.path().join("camp.toml"),
            format!(
                "{BASE_TOML}[[order]]\nname=\"new\"\non=\"cron:0 8 * * *\"\nformula=\"one-step\"\n"
            ),
        )
        .unwrap();
        let input = rt.reload_if_changed(now).unwrap().unwrap();
        assert_eq!(input.kind, EventType::ConfigChanged);
        assert_eq!(input.data["applied"], true);
        assert_eq!(input.data["orders"], 1);
        assert!(rt.order("new").is_some());
        assert!(rt.poll_timeout(now).is_some(), "the reload armed the heap");
        // a broken edit → rejected, old config retained
        std::fs::write(dir.path().join("camp.toml"), "junk [[[").unwrap();
        let input = rt.reload_if_changed(now).unwrap().unwrap();
        assert_eq!(input.data["applied"], false);
        assert!(!input.data["error"].as_str().unwrap().is_empty());
        assert!(rt.order("new").is_some(), "old config retained");
    }

    /// PR #13 review MEDIUM 3: an infrastructure error mid-cook-list must
    /// not lose the taken cooks — the cursor is already past their
    /// order.fired events, so without a requeue they would never cook
    /// until a restart's reconciliation. Injection: a raw second
    /// connection installs a trigger that aborts order.failed inserts, so
    /// execute_fire's failure-eventing (an order with a missing formula)
    /// becomes an infra error.
    #[test]
    fn an_infra_error_mid_cook_list_requeues_the_survivors() {
        let (dir, mut ledger) =
            fixture("[[order]]\nname=\"broken\"\non=\"cron:0 0 1 1 *\"\nformula=\"no-such\"\n");
        let mut rt = runtime(&dir, "2026-07-06T07:20:00Z");
        ledger
            .append(fired_input("broken", &FireCause::Manual))
            .unwrap();
        ledger
            .append(fired_input("broken", &FireCause::Manual))
            .unwrap();

        let raw = rusqlite::Connection::open(dir.path().join("camp.db")).unwrap();
        raw.execute_batch(
            "CREATE TRIGGER inject_infra_error BEFORE INSERT ON events
             WHEN NEW.type = 'order.failed'
             BEGIN SELECT RAISE(ABORT, 'injected infrastructure error'); END;",
        )
        .unwrap();

        let mut readiness = ReadinessProcessor::default();
        let err = settle(&mut ledger, &mut readiness, &mut rt, &clock());
        assert!(err.is_err(), "the infra error must surface");
        // BOTH cooks survive for the next settle (the failing one included)
        assert_eq!(rt.pending_cook_count(), 2);

        // infrastructure recovers → the next settle drains them
        raw.execute_batch("DROP TRIGGER inject_infra_error").unwrap();
        settle(&mut ledger, &mut readiness, &mut rt, &clock()).unwrap();
        assert_eq!(rt.pending_cook_count(), 0);
        assert_eq!(
            ledger.events_of_type(EventType::OrderFailed).unwrap().len(),
            2,
            "both fires resolve once the ledger writes again"
        );
    }

    /// PR #13 review MEDIUM 2: the downtime catch-up anchor is the last
    /// instant campd OBSERVED (its cursor position), never the ledger's
    /// last event of any actor — a daemon-less CLI write between a missed
    /// fire and campd's start must not mask the miss (spec §9: "missed
    /// fires within catch_up_window fire once on wake").
    #[test]
    fn downtime_catch_up_survives_an_intervening_cli_write() {
        use camp_core::clock::FixedClock;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("camp.toml"),
            format!(
                "{BASE_TOML}[[order]]\nname=\"hourly\"\non=\"cron:0 * * * *\"\nformula=\"one-step\"\n"
            ),
        )
        .unwrap();
        let db = dir.path().join("camp.db");

        // campd's last life: processed through campd.started at 06:50.
        {
            let mut ledger =
                Ledger::open_with_clock(&db, Box::new(FixedClock::new("2026-07-06T06:50:00Z")))
                    .unwrap();
            ledger
                .append(EventInput {
                    kind: EventType::CampdStarted,
                    rig: None,
                    actor: "campd".into(),
                    bead: None,
                    data: serde_json::json!({}),
                })
                .unwrap();
            ledger
                .process_past_cursor(super::super::cursor::CAMPD_CURSOR, &mut |_c, _e| Ok(()))
                .unwrap();
        }
        // campd dies; the 07:00 fire is missed; a daemon-less CLI write
        // lands at 07:30 (campd's cursor never advances past it).
        let mut ledger =
            Ledger::open_with_clock(&db, Box::new(FixedClock::new("2026-07-06T07:30:00Z")))
                .unwrap();
        ledger
            .append(EventInput {
                kind: EventType::BeadCreated,
                rig: Some("gc".into()),
                actor: "cli".into(),
                bead: Some("gc-1".into()),
                data: serde_json::json!({"title":"intervening write"}),
            })
            .unwrap();

        // campd restarts at 07:40: the anchor is 06:50 (cursor position),
        // NOT 07:30 (last event), so recompute catches the 07:00 fire.
        let now = ts("2026-07-06T07:40:00Z");
        let anchor = catch_up_anchor(&ledger, now).unwrap();
        assert_eq!(anchor, ts("2026-07-06T06:50:00Z"));
        let mut rt = OrdersRuntime::build(dir.path(), now, TimeZone::UTC).unwrap();
        let catch_ups = rt.recompute(now, anchor);
        assert_eq!(catch_ups.len(), 1, "{catch_ups:?}");
        assert_eq!(catch_ups[0].order, "hourly");
        assert_eq!(catch_ups[0].scheduled, ts("2026-07-06T07:00:00Z"));
        // the masked-anchor counterexample: anchored on the CLI write's ts,
        // the missed fire would be invisible
        assert!(rt.recompute(now, ts("2026-07-06T07:30:00Z")).is_empty());
    }

    /// A fresh camp (cursor at 0) has nothing to catch up: anchor == now.
    #[test]
    fn catch_up_anchor_is_now_for_an_unprocessed_ledger() {
        let (_dir, ledger) = fixture("");
        let now = ts("2026-07-06T07:40:00Z");
        assert_eq!(catch_up_anchor(&ledger, now).unwrap(), now);
    }

    #[test]
    fn startup_reconciliation_cooks_orphaned_fires() {
        let (dir, mut ledger) =
            fixture("[[order]]\nname=\"one-shot\"\non=\"cron:0 0 1 1 *\"\nformula=\"one-step\"\n");
        let mut rt = runtime(&dir, "2026-07-06T07:20:00Z");
        // A fire whose cook was lost: cursor advanced past it (simulated
        // kill -9 between order.fired and the cook).
        ledger
            .append(fired_input("one-shot", &FireCause::Manual))
            .unwrap();
        ledger
            .process_past_cursor(super::super::cursor::CAMPD_CURSOR, &mut |_conn, _event| {
                Ok(())
            })
            .unwrap();
        // settle alone sees nothing (cursor is past the fire)…
        settle_all(&mut ledger, &mut rt);
        assert!(
            ledger
                .events_of_type(EventType::RunCooked)
                .unwrap()
                .is_empty()
        );
        // …reconciliation queues it, the next settle cooks it, exactly once
        for cook in camp_core::orders::unresponded_fires(&ledger).unwrap() {
            rt.queue_cook(cook);
        }
        settle_all(&mut ledger, &mut rt);
        assert_eq!(
            ledger.events_of_type(EventType::RunCooked).unwrap().len(),
            1
        );
        // repeating reconciliation cooks nothing further
        for cook in camp_core::orders::unresponded_fires(&ledger).unwrap() {
            rt.queue_cook(cook);
        }
        settle_all(&mut ledger, &mut rt);
        assert_eq!(
            ledger.events_of_type(EventType::RunCooked).unwrap().len(),
            1
        );
    }
}

//! Orders (spec §9): cron- and event-triggered formulas. The cron machinery
//! is a timer heap, never a tick (invariant 1). Grows over the Phase 10
//! tasks: cron engine and heap (`cron`), `[[order]]` compilation (`parse`),
//! and the fire pipeline (here).

pub mod cron;
pub mod parse;

use std::path::{Path, PathBuf};

use jiff::Timestamp;
use rusqlite::Connection;

use crate::Seq;
use crate::error::CoreError;
use crate::event::{Event, EventInput, EventType};
use crate::ledger::Ledger;
use cron::CronExpr;

/// What trips an order (spec §9): a cron schedule or an event pattern.
#[derive(Debug, Clone, PartialEq)]
pub enum Trigger {
    Cron {
        expr: CronExpr,
    },
    Event {
        event_type: String,
        label: Option<String>,
    },
}

/// One compiled `[[order]]` table (spec §9).
#[derive(Debug, Clone, PartialEq)]
pub struct Order {
    pub name: String,
    pub trigger: Trigger,
    pub formula: String,
    pub rig: Option<String>,
    /// Missed-fire catch-up window (spec §9): default 2h; `Duration::ZERO`
    /// (config `"0"`) disables catch-up.
    pub catch_up_window: std::time::Duration,
}

/// Cook events for an order-fired run carry `actor =
/// "order:<name>:<fired-seq>"` (plan Decision J) — the cause chain from a
/// run back to its firing, in the mold of spec §7.2's `session:<id>`
/// actors. Order names are validated to `^[a-z0-9][a-z0-9_-]*$` so the
/// encoding parses unambiguously.
pub const ORDER_ACTOR_PREFIX: &str = "order:";

/// Why an order fired — recorded verbatim in `order.fired` data. All three
/// causes flow through the same pipeline: append `order.fired`, then campd
/// cooks by *processing* it (plan Decision D — away-mode, event triggers,
/// and `camp order run` are one code path).
///
/// HAZARD, documented not guarded (plan Decision I): an event order whose
/// own firing produces events matching its trigger (`event:order.fired`,
/// or `event:bead.created` matching cooked beads) recurses without bound —
/// visibly, in the ledger, exactly as a `* * * * *` cron on an expensive
/// formula would. campd executes declared structure (spec §8.3); the
/// declaration is the user's power and the user's responsibility.
#[derive(Debug, Clone, PartialEq)]
pub enum FireCause {
    Cron {
        scheduled: Timestamp,
        catch_up: bool,
    },
    Event {
        cause_seq: Seq,
    },
    Manual,
}

/// A fire awaiting its cook: queued by the processor when it sees
/// `order.fired`, executed by campd's settle loop.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingCook {
    pub order: String,
    pub fired_seq: Seq,
}

/// The canonical spec §7.2 timestamp form (RFC3339 UTC whole seconds) for
/// event data fields.
fn canonical_ts(ts: Timestamp) -> String {
    ts.strftime("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// The `order.fired` declaration for a trigger trip. Cron and event fires
/// come from campd (`actor: "campd"`); manual fires from the CLI
/// (`actor: "cli"`).
pub fn fired_input(order_name: &str, cause: &FireCause) -> EventInput {
    let (actor, data) = match cause {
        FireCause::Cron {
            scheduled,
            catch_up,
        } => {
            let mut data = serde_json::json!({
                "order": order_name,
                "trigger": "cron",
                "scheduled_ts": canonical_ts(*scheduled),
            });
            if *catch_up {
                data["catch_up"] = serde_json::json!(true);
            }
            ("campd", data)
        }
        FireCause::Event { cause_seq } => (
            "campd",
            serde_json::json!({"order": order_name, "trigger": "event", "cause_seq": cause_seq}),
        ),
        FireCause::Manual => (
            "cli",
            serde_json::json!({"order": order_name, "trigger": "manual"}),
        ),
    };
    EventInput {
        kind: EventType::OrderFired,
        rig: None,
        actor: actor.into(),
        bead: None,
        data,
    }
}

/// `"order:<name>:<fired-seq>"` — the actor for every event a fired
/// order's cook produces.
pub fn cook_actor(order_name: &str, fired_seq: Seq) -> String {
    format!("{ORDER_ACTOR_PREFIX}{order_name}:{fired_seq}")
}

/// Invert `cook_actor`. `None` for any actor not in the encoding.
pub fn parse_cook_actor(actor: &str) -> Option<(&str, Seq)> {
    let rest = actor.strip_prefix(ORDER_ACTOR_PREFIX)?;
    let (name, seq) = rest.rsplit_once(':')?;
    if name.is_empty() {
        return None;
    }
    Some((name, seq.parse().ok()?))
}

/// The processor's `order.fired` reaction: queue the cook. `None` for any
/// other event type.
pub fn pending_cook_from_fired(event: &Event) -> Result<Option<PendingCook>, CoreError> {
    if event.kind != EventType::OrderFired {
        return Ok(None);
    }
    let order = event
        .data
        .get("order")
        .and_then(|v| v.as_str())
        .ok_or_else(|| CoreError::InvalidEventData {
            event_type: event.kind.as_str().to_owned(),
            reason: "order.fired without an order name".to_owned(),
        })?;
    Ok(Some(PendingCook {
        order: order.to_owned(),
        fired_seq: event.seq,
    }))
}

/// Does `event` trip this event-triggered order? The type must match; a
/// label filter additionally requires the event's bead to carry the label
/// (spec §9: the label filter matches bead.* events whose bead carries the
/// label). Evaluated once per committed event on the processing path —
/// zero standing cost.
pub fn event_trigger_matches(
    conn: &Connection,
    order: &Order,
    event: &Event,
) -> Result<bool, CoreError> {
    let Trigger::Event { event_type, label } = &order.trigger else {
        return Ok(false);
    };
    if event.kind.as_str() != event_type {
        return Ok(false);
    }
    let Some(label) = label else { return Ok(true) };
    let Some(bead) = event.bead.as_deref() else {
        return Ok(false);
    };
    use rusqlite::OptionalExtension;
    let labels: Option<String> = conn
        .query_row("SELECT labels FROM beads WHERE id = ?1", [bead], |r| {
            r.get(0)
        })
        .optional()?;
    let Some(labels) = labels else {
        return Ok(false);
    };
    let labels: Vec<String> = serde_json::from_str(&labels)?;
    Ok(labels.iter().any(|l| l == label))
}

/// The completion event for a `bead.closed`, if the closed bead is the
/// root of an order-cooked run (plan Decision C): `order.completed` on
/// pass, run-shaped `order.failed` on fail, `None` otherwise. Roots are
/// beads with `run_id` set and no `step_id` (Phase 5's cook shape); the
/// owning order comes from the run's `run.cooked` actor. Appended by the
/// processor via `Ledger::append_on` — atomic with the cursor advance.
pub fn completion_input(conn: &Connection, event: &Event) -> Result<Option<EventInput>, CoreError> {
    if event.kind != EventType::BeadClosed {
        return Ok(None);
    }
    let Some(bead) = event.bead.as_deref() else {
        return Ok(None);
    };
    use rusqlite::OptionalExtension;
    let ids: Option<(Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT run_id, step_id FROM beads WHERE id = ?1",
            [bead],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((Some(run_id), None)) = ids else {
        return Ok(None); // not a run root
    };
    let cooked_actor: Option<String> = conn
        .query_row(
            "SELECT actor FROM events WHERE bead = ?1 AND type = 'run.cooked'
             ORDER BY seq DESC LIMIT 1",
            [bead],
            |r| r.get(0),
        )
        .optional()?;
    let Some((order, fired_seq)) = cooked_actor.as_deref().and_then(parse_cook_actor) else {
        return Ok(None); // a run, but not an order's run
    };
    let outcome = event
        .data
        .get("outcome")
        .and_then(|v| v.as_str())
        .ok_or_else(|| CoreError::InvalidEventData {
            event_type: event.kind.as_str().to_owned(),
            reason: "bead.closed without an outcome".to_owned(),
        })?;
    let (kind, data) = if outcome == "pass" {
        (
            EventType::OrderCompleted,
            serde_json::json!({
                "order": order, "fired_seq": fired_seq, "root_bead": bead,
                "run_id": run_id, "outcome": "pass",
            }),
        )
    } else {
        (
            EventType::OrderFailed,
            serde_json::json!({
                "order": order, "fired_seq": fired_seq, "root_bead": bead,
                "run_id": run_id, "outcome": "fail",
            }),
        )
    };
    Ok(Some(EventInput {
        kind,
        rig: event.rig.clone(),
        actor: "campd".into(),
        bead: None,
        data,
    }))
}

/// Where an order's formula name resolves (spec §7.1/§9, plan Decision E):
/// `<camp>/formulas/<name>.toml`. Phase 12's pack layering replaces this
/// body; local definitions stay the highest layer (spec §11).
pub fn formula_path(camp_root: &Path, formula: &str) -> PathBuf {
    camp_root.join("formulas").join(format!("{formula}.toml"))
}

/// Every `fired_seq` that already has a response: a `run.cooked` whose
/// actor encodes it, or an `order.failed` carrying it. Backs cook dedupe
/// and startup reconciliation; order-event counts are small.
fn responded_fired_seqs(ledger: &Ledger) -> Result<std::collections::BTreeSet<Seq>, CoreError> {
    let mut responded = std::collections::BTreeSet::new();
    for cooked in ledger.events_of_type(EventType::RunCooked)? {
        if let Some((_, fired_seq)) = parse_cook_actor(&cooked.actor) {
            responded.insert(fired_seq);
        }
    }
    for failed in ledger.events_of_type(EventType::OrderFailed)? {
        if let Some(fired_seq) = failed.data.get("fired_seq").and_then(|v| v.as_i64()) {
            responded.insert(fired_seq);
        }
    }
    Ok(responded)
}

/// Has this fire already been answered (cooked, or failed with an event)?
pub fn fire_response_exists(ledger: &Ledger, fired_seq: Seq) -> Result<bool, CoreError> {
    Ok(responded_fired_seqs(ledger)?.contains(&fired_seq))
}

/// `order.fired` events with no response — fires orphaned by a crash
/// between the declaration and its cook. campd cooks them at startup
/// (plan Decision D: observation over state, kill -9 self-heals).
pub fn unresponded_fires(ledger: &Ledger) -> Result<Vec<PendingCook>, CoreError> {
    let responded = responded_fired_seqs(ledger)?;
    let mut pending = Vec::new();
    for fired in ledger.events_of_type(EventType::OrderFired)? {
        if !responded.contains(&fired.seq)
            && let Some(cook) = pending_cook_from_fired(&fired)?
        {
            pending.push(cook);
        }
    }
    Ok(pending)
}

/// Execute the cook for a declared fire: resolve the formula file, parse
/// and validate it, resolve the rig, and cook with the order actor.
///
/// Returns `Ok(Some(run))` on success, `Ok(None)` when the fire already
/// has a response (dedupe — replay-safe) or when an ORDER-level failure
/// occurred (missing/invalid formula, unresolvable rig, cook error): those
/// are appended as `order.failed {order, fired_seq, error}` and never take
/// campd down (plan Decision K). Only infrastructure failures — the ledger
/// refusing the failure event itself — return `Err`.
pub fn execute_fire(
    ledger: &mut Ledger,
    config: &crate::config::CampConfig,
    camp_root: &Path,
    order: &Order,
    fired_seq: Seq,
) -> Result<Option<crate::formula::CookedRun>, CoreError> {
    if fire_response_exists(ledger, fired_seq)? {
        return Ok(None);
    }
    let fail = |ledger: &mut Ledger, error: String| -> Result<(), CoreError> {
        ledger.append(EventInput {
            kind: EventType::OrderFailed,
            rig: None,
            actor: "campd".into(),
            bead: None,
            data: serde_json::json!({
                "order": order.name, "fired_seq": fired_seq, "error": error,
            }),
        })?;
        Ok(())
    };
    let path = formula_path(camp_root, &order.formula);
    if !path.exists() {
        fail(
            ledger,
            format!(
                "formula {:?} not found at {}",
                order.formula,
                path.display()
            ),
        )?;
        return Ok(None);
    }
    let formula = match crate::formula::parse_and_validate(&path) {
        Ok(formula) => formula,
        Err(e) => {
            fail(ledger, format!("formula {:?}: {e}", order.formula))?;
            return Ok(None);
        }
    };
    let rig = match resolve_rig(config, order) {
        Ok(rig) => rig.clone(),
        Err(reason) => {
            fail(ledger, reason)?;
            return Ok(None);
        }
    };
    match crate::formula::cook(
        ledger,
        &formula,
        &camp_root.join("runs"),
        &rig,
        &cook_actor(&order.name, fired_seq),
    ) {
        Ok(run) => Ok(Some(run)),
        Err(e) => {
            fail(ledger, format!("cook failed: {e}"))?;
            Ok(None)
        }
    }
}

/// The `cmd/create` rig rule at fire time (plan Decision M): the order's
/// explicit rig, else the sole configured rig, else an error naming the
/// fix.
fn resolve_rig<'a>(
    config: &'a crate::config::CampConfig,
    order: &Order,
) -> Result<&'a crate::config::RigConfig, String> {
    match &order.rig {
        Some(name) => config
            .rig(name)
            .map_err(|_| format!("field \"rig\": unknown rig {name:?}")),
        None => match config.rigs.as_slice() {
            [only] => Ok(only),
            [] => Err("no rigs configured; run camp rig add <path> first".into()),
            _ => Err("multiple rigs configured; set rig on the order".into()),
        },
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::event::{EventInput, EventType};
    use crate::ledger::Ledger;

    fn ts(s: &str) -> jiff::Timestamp {
        s.parse().unwrap()
    }

    fn test_ledger() -> (tempfile::TempDir, Ledger) {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::open(&dir.path().join("camp.db")).unwrap();
        (dir, ledger)
    }

    fn append_with(
        ledger: &mut Ledger,
        kind: EventType,
        bead: Option<&str>,
        actor: &str,
        data: serde_json::Value,
    ) {
        ledger
            .append(EventInput {
                kind,
                rig: Some("gc".into()),
                actor: actor.into(),
                bead: bead.map(Into::into),
                data,
            })
            .unwrap();
    }

    fn append_created(ledger: &mut Ledger, id: &str, labels: &[&str]) {
        append_with(
            ledger,
            EventType::BeadCreated,
            Some(id),
            "test",
            serde_json::json!({"title": id, "labels": labels}),
        );
    }

    fn append_closed(ledger: &mut Ledger, id: &str, outcome: &str) {
        append_with(
            ledger,
            EventType::BeadClosed,
            Some(id),
            "test",
            serde_json::json!({"outcome": outcome}),
        );
    }

    fn order_on(on: &str) -> Order {
        let cfg = crate::config::CampConfig::parse(&format!(
            "[camp]\nname=\"d\"\n[[order]]\nname=\"t\"\non=\"{on}\"\nformula=\"f\"\n"
        ))
        .unwrap();
        parse::compile_orders(&cfg).unwrap().remove(0)
    }

    #[test]
    fn fired_inputs_carry_the_cause() {
        let cron = fired_input(
            "t",
            &FireCause::Cron {
                scheduled: ts("2026-07-06T07:00:00Z"),
                catch_up: true,
            },
        );
        assert_eq!(cron.kind, EventType::OrderFired);
        assert_eq!(cron.actor, "campd");
        assert_eq!(cron.data["trigger"], "cron");
        assert_eq!(cron.data["scheduled_ts"], "2026-07-06T07:00:00Z");
        assert_eq!(cron.data["catch_up"], true);
        let ev = fired_input("t", &FireCause::Event { cause_seq: 9 });
        assert_eq!(ev.data["trigger"], "event");
        assert_eq!(ev.data["cause_seq"], 9);
        let manual = fired_input("t", &FireCause::Manual);
        assert_eq!(manual.actor, "cli");
        assert_eq!(manual.data["trigger"], "manual");
    }

    #[test]
    fn on_time_cron_fire_omits_catch_up_flag() {
        let cron = fired_input(
            "t",
            &FireCause::Cron {
                scheduled: ts("2026-07-06T07:00:00Z"),
                catch_up: false,
            },
        );
        assert!(
            cron.data.get("catch_up").is_none(),
            "on-time fires carry no catch_up key"
        );
    }

    #[test]
    fn cook_actor_round_trips() {
        let actor = cook_actor("morning-triage", 412);
        assert_eq!(actor, "order:morning-triage:412");
        assert_eq!(parse_cook_actor(&actor), Some(("morning-triage", 412)));
        assert_eq!(parse_cook_actor("session:8f3c2e01"), None);
        assert_eq!(parse_cook_actor("order:name-without-seq"), None);
    }

    #[test]
    fn pending_cook_comes_from_fired_events_only() {
        let (_dir, mut ledger) = test_ledger();
        ledger.append(fired_input("t", &FireCause::Manual)).unwrap();
        append_created(&mut ledger, "gc-1", &[]);
        let mut cooks = Vec::new();
        ledger
            .process_past_cursor("t", &mut |_conn, event| {
                if let Some(cook) = pending_cook_from_fired(event)? {
                    cooks.push(cook);
                }
                Ok(())
            })
            .unwrap();
        assert_eq!(
            cooks,
            vec![PendingCook {
                order: "t".into(),
                fired_seq: 1
            }]
        );
    }

    #[test]
    fn event_trigger_matches_type_and_bead_label() {
        let (_dir, mut ledger) = test_ledger();
        // gc-1 labeled ci-red, gc-2 unlabeled
        append_created(&mut ledger, "gc-1", &["ci-red"]);
        append_created(&mut ledger, "gc-2", &[]);
        append_closed(&mut ledger, "gc-1", "pass");
        append_closed(&mut ledger, "gc-2", "pass");
        let labeled = order_on("event:bead.closed[label=ci-red]");
        let unlabeled = order_on("event:bead.closed");
        let mut results = Vec::new();
        ledger
            .process_past_cursor("t", &mut |conn, event| {
                results.push((
                    event.seq,
                    event_trigger_matches(conn, &labeled, event).unwrap(),
                    event_trigger_matches(conn, &unlabeled, event).unwrap(),
                ));
                Ok(())
            })
            .unwrap();
        // seq 1/2 = creates (wrong type: no match), 3 = close gc-1 (both
        // match), 4 = close gc-2 (only the unlabeled order matches)
        assert_eq!(results[0], (1, false, false));
        assert_eq!(results[2], (3, true, true));
        assert_eq!(results[3], (4, false, true));
    }

    #[test]
    fn completion_input_fires_only_for_order_cooked_run_roots() {
        let (_dir, mut ledger) = test_ledger();
        // Simulate a cooked run the way cook() writes it (run_id on both
        // beads, step_id only on the step), with the order cook actor:
        let actor = cook_actor("t", 1);
        append_with(
            &mut ledger,
            EventType::BeadCreated,
            Some("gc-1"),
            &actor,
            serde_json::json!({"title":"root","run_id":"r1","needs":["gc-2"]}),
        );
        append_with(
            &mut ledger,
            EventType::BeadCreated,
            Some("gc-2"),
            &actor,
            serde_json::json!({"title":"step","run_id":"r1","step_id":"s1"}),
        );
        append_with(
            &mut ledger,
            EventType::RunCooked,
            Some("gc-1"),
            &actor,
            serde_json::json!({"run_id":"r1","formula":"f","root":"gc-1","steps":{"s1":"gc-2"}}),
        );
        // a plain bead, closed: no completion
        append_created(&mut ledger, "gc-3", &[]);
        append_closed(&mut ledger, "gc-3", "pass");
        // the STEP closing: no completion (step_id set)
        append_closed(&mut ledger, "gc-2", "pass");
        // the ROOT closing with fail: order.failed with the run shape
        append_closed(&mut ledger, "gc-1", "fail");
        let mut completions = Vec::new();
        ledger
            .process_past_cursor("t", &mut |conn, event| {
                if let Some(input) = completion_input(conn, event).unwrap() {
                    completions.push(input);
                }
                Ok(())
            })
            .unwrap();
        assert_eq!(completions.len(), 1);
        let c = &completions[0];
        assert_eq!(c.kind, EventType::OrderFailed);
        assert_eq!(c.actor, "campd");
        assert_eq!(c.data["order"], "t");
        assert_eq!(c.data["fired_seq"], 1);
        assert_eq!(c.data["root_bead"], "gc-1");
        assert_eq!(c.data["run_id"], "r1");
        assert_eq!(c.data["outcome"], "fail");
    }

    #[test]
    fn completion_input_reports_pass_as_order_completed() {
        let (_dir, mut ledger) = test_ledger();
        let actor = cook_actor("t", 1);
        append_with(
            &mut ledger,
            EventType::BeadCreated,
            Some("gc-1"),
            &actor,
            serde_json::json!({"title":"root","run_id":"r1"}),
        );
        append_with(
            &mut ledger,
            EventType::RunCooked,
            Some("gc-1"),
            &actor,
            serde_json::json!({"run_id":"r1","formula":"f","root":"gc-1","steps":{}}),
        );
        append_closed(&mut ledger, "gc-1", "pass");
        let mut completions = Vec::new();
        ledger
            .process_past_cursor("t", &mut |conn, event| {
                if let Some(input) = completion_input(conn, event).unwrap() {
                    completions.push(input);
                }
                Ok(())
            })
            .unwrap();
        assert_eq!(completions.len(), 1);
        assert_eq!(completions[0].kind, EventType::OrderCompleted);
        assert_eq!(completions[0].data["outcome"], "pass");
    }

    #[test]
    fn non_order_cooked_runs_yield_no_completion() {
        let (_dir, mut ledger) = test_ledger();
        // same shape, but a plain (non-order) actor cooked it
        append_with(
            &mut ledger,
            EventType::BeadCreated,
            Some("gc-1"),
            "cli",
            serde_json::json!({"title":"root","run_id":"r1"}),
        );
        append_with(
            &mut ledger,
            EventType::RunCooked,
            Some("gc-1"),
            "cli",
            serde_json::json!({"run_id":"r1","formula":"f","root":"gc-1","steps":{}}),
        );
        append_closed(&mut ledger, "gc-1", "pass");
        let mut completions = Vec::new();
        ledger
            .process_past_cursor("t", &mut |conn, event| {
                if let Some(input) = completion_input(conn, event).unwrap() {
                    completions.push(input);
                }
                Ok(())
            })
            .unwrap();
        assert!(completions.is_empty());
    }

    #[test]
    fn formula_path_is_the_camp_local_formulas_dir() {
        assert_eq!(
            formula_path(std::path::Path::new("/camp/.camp"), "triage-inbox"),
            std::path::PathBuf::from("/camp/.camp/formulas/triage-inbox.toml")
        );
    }

    // ---- execute_fire + reconciliation (Task 10.8; needs Phase 5's cook)

    fn camp_fixture() -> (tempfile::TempDir, Ledger, crate::config::CampConfig) {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::open(&dir.path().join("camp.db")).unwrap();
        let config = crate::config::CampConfig::parse(
            "[camp]\nname=\"d\"\n\n[[rigs]]\nname=\"gc\"\npath=\"/p\"\nprefix=\"gc\"\n",
        )
        .unwrap();
        (dir, ledger, config)
    }

    fn write_formula(camp_root: &Path, name: &str) {
        std::fs::create_dir_all(camp_root.join("formulas")).unwrap();
        std::fs::write(
            camp_root.join("formulas").join(format!("{name}.toml")),
            format!("formula = \"{name}\"\n\n[[steps]]\nid = \"s1\"\ntitle = \"one step\"\n"),
        )
        .unwrap();
    }

    fn cron_order_named(name: &str, formula: &str) -> Order {
        Order {
            name: name.into(),
            trigger: Trigger::Cron {
                expr: CronExpr::parse("0 0 1 1 *").unwrap(),
            },
            formula: formula.into(),
            rig: None,
            catch_up_window: std::time::Duration::from_secs(2 * 60 * 60),
        }
    }

    #[test]
    fn execute_fire_cooks_the_formula_with_the_order_actor() {
        let (dir, mut ledger, config) = camp_fixture();
        write_formula(dir.path(), "one-step");
        let order = cron_order_named("t", "one-step");
        let fired = ledger.append(fired_input("t", &FireCause::Manual)).unwrap();
        let run = execute_fire(&mut ledger, &config, dir.path(), &order, fired)
            .unwrap()
            .unwrap();
        // cook events carry the order actor; run dir pinned under <camp>/runs/
        let cooked = ledger.events_of_type(EventType::RunCooked).unwrap();
        assert_eq!(cooked.len(), 1);
        assert_eq!(cooked[0].actor, cook_actor("t", fired));
        assert!(
            dir.path()
                .join("runs")
                .join(&run.run_id)
                .join("manifest.json")
                .exists()
        );
        // dedupe: a second execution for the same fired_seq is a no-op
        assert!(
            execute_fire(&mut ledger, &config, dir.path(), &order, fired)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            ledger.events_of_type(EventType::RunCooked).unwrap().len(),
            1
        );
    }

    #[test]
    fn execute_fire_failure_is_evented_not_thrown() {
        let (dir, mut ledger, config) = camp_fixture();
        let order = cron_order_named("t", "missing-formula");
        let fired = ledger.append(fired_input("t", &FireCause::Manual)).unwrap();
        assert!(
            execute_fire(&mut ledger, &config, dir.path(), &order, fired)
                .unwrap()
                .is_none()
        );
        let failed = ledger.events_of_type(EventType::OrderFailed).unwrap();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].data["fired_seq"], fired);
        assert!(
            failed[0].data["error"]
                .as_str()
                .unwrap()
                .contains("missing-formula")
        );
    }

    #[test]
    fn execute_fire_with_no_resolvable_rig_is_evented() {
        let (dir, mut ledger, _config) = camp_fixture();
        write_formula(dir.path(), "one-step");
        let riglss = crate::config::CampConfig::parse("[camp]\nname=\"d\"\n").unwrap();
        let order = cron_order_named("t", "one-step");
        let fired = ledger.append(fired_input("t", &FireCause::Manual)).unwrap();
        assert!(
            execute_fire(&mut ledger, &riglss, dir.path(), &order, fired)
                .unwrap()
                .is_none()
        );
        let failed = ledger.events_of_type(EventType::OrderFailed).unwrap();
        assert_eq!(failed.len(), 1);
        assert!(failed[0].data["error"].as_str().unwrap().contains("rig"));
    }

    #[test]
    fn unresponded_fires_reconciles_exactly_the_unanswered_ones() {
        let (dir, mut ledger, config) = camp_fixture();
        write_formula(dir.path(), "one-step");
        let order = cron_order_named("t", "one-step");
        let answered = ledger.append(fired_input("t", &FireCause::Manual)).unwrap();
        execute_fire(&mut ledger, &config, dir.path(), &order, answered).unwrap();
        let orphaned = ledger.append(fired_input("t", &FireCause::Manual)).unwrap();
        let failed = ledger.append(fired_input("u", &FireCause::Manual)).unwrap();
        ledger
            .append(EventInput {
                kind: EventType::OrderFailed,
                rig: None,
                actor: "campd".into(),
                bead: None,
                data: serde_json::json!({"order":"u","fired_seq":failed,"error":"e"}),
            })
            .unwrap();
        assert_eq!(
            unresponded_fires(&ledger).unwrap(),
            vec![PendingCook {
                order: "t".into(),
                fired_seq: orphaned
            }]
        );
    }
}

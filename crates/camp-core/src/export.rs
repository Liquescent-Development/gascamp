//! `camp export --city <dir>` (spec §15.3): graduation is an export, not a
//! backend. Everything here is read-only — over the ledger and the camp
//! directory. Camp never writes into a live city's store, and export
//! appends nothing to camp's own ledger. Field-level mapping tables:
//! docs/reference/export.md.

use std::collections::BTreeMap;

use rusqlite::Connection;

use crate::error::CoreError;
use crate::orders::parse::OrderConfig;
use crate::orders::{Order, Trigger};

/// One bead with every column `beads.jsonl` needs — the full-fidelity
/// superset of [`crate::readiness::BeadRow`] plus the `needs` edges from
/// `deps`. Creation order (`ORDER BY created_ts, id`), read-only.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportBead {
    pub id: String,
    pub rig: String,
    pub kind: String,
    pub title: String,
    pub description: String,
    pub status: String,
    pub assignee: Option<String>,
    pub claimed_by: Option<String>,
    pub outcome: Option<String>,
    pub close_reason: Option<String>,
    pub labels: Vec<String>,
    pub run_id: Option<String>,
    pub step_id: Option<String>,
    pub needs: Vec<String>,
    pub created_ts: String,
    pub updated_ts: String,
    pub closed_ts: Option<String>,
}

pub(crate) fn export_beads(conn: &Connection) -> Result<Vec<ExportBead>, CoreError> {
    let mut needs_by_bead: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut dep_stmt =
        conn.prepare("SELECT bead_id, needs_id FROM deps ORDER BY bead_id, needs_id")?;
    let dep_rows =
        dep_stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    for row in dep_rows {
        let (bead_id, needs_id) = row?;
        needs_by_bead.entry(bead_id).or_default().push(needs_id);
    }

    let mut stmt = conn.prepare(
        "SELECT id, rig, type, title, description, status, assignee, claimed_by,
                outcome, close_reason, labels, run_id, step_id, created_ts,
                updated_ts, closed_ts
         FROM beads ORDER BY created_ts, id",
    )?;
    let rows = stmt.query_map([], |row| {
        let labels_json: String = row.get(10)?;
        let labels: Vec<String> = serde_json::from_str(&labels_json).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(10, rusqlite::types::Type::Text, Box::new(e))
        })?;
        Ok(ExportBead {
            id: row.get(0)?,
            rig: row.get(1)?,
            kind: row.get(2)?,
            title: row.get(3)?,
            description: row.get(4)?,
            status: row.get(5)?,
            assignee: row.get(6)?,
            claimed_by: row.get(7)?,
            outcome: row.get(8)?,
            close_reason: row.get(9)?,
            labels,
            run_id: row.get(11)?,
            step_id: row.get(12)?,
            needs: Vec::new(),
            created_ts: row.get(13)?,
            updated_ts: row.get(14)?,
            closed_ts: row.get(15)?,
        })
    })?;
    let mut beads = Vec::new();
    for row in rows {
        let mut bead = row?;
        if let Some(needs) = needs_by_bead.remove(&bead.id) {
            bead.needs = needs;
        }
        beads.push(bead);
    }
    Ok(beads)
}

/// One issue line of `beads.jsonl` — the bd import/export wire format
/// (beadslib `types.Issue`, v1.0.4 at the gascity pin; the format
/// `bd import` actually reads, NOT Gas City's internal exec-provider
/// shape, whose `parent`/`needs`/`ref` fields bd silently drops).
/// Serialize-only: camp emits, bd consumes. Field-level mapping:
/// docs/reference/export.md.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct BdIssue {
    /// bd's own export tags issue lines; absence also means issue.
    #[serde(rename = "_type")]
    pub record: &'static str,
    pub id: String,
    pub title: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub description: String,
    pub status: String,
    /// bd priority 0 means P0/critical and the field carries no omitempty
    /// in bd's export; absent priority is NOT defaulted at import — camp
    /// has no priority, so every line says 2 (normal) explicitly.
    pub priority: i64,
    pub issue_type: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub close_reason: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<BdDependency>,
    #[serde(skip_serializing_if = "serde_json::Map::is_empty")]
    pub metadata: serde_json::Map<String, serde_json::Value>,
}

/// One `dependencies` entry: camp's `needs` edge is a readiness-blocking
/// dependency, bd's `blocks` type (in bd's blocking-for-ready set).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct BdDependency {
    pub issue_id: String,
    pub depends_on_id: String,
    #[serde(rename = "type")]
    pub dep_type: &'static str,
}

/// A native bd memory record: `bd import` stores these as `bd remember`
/// KV entries, not issues. key = the camp bead id, value = the fact.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct BdMemory {
    #[serde(rename = "_type")]
    pub record: &'static str,
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BdRecord {
    Issue(Box<BdIssue>),
    Memory(BdMemory),
}

/// Map one camp bead to its `beads.jsonl` record. Field-level table:
/// docs/reference/export.md.
///
/// Golden-output coupling, deliberate: `serde_json::Map` is a BTreeMap
/// (alphabetical key order) unless serde_json's `preserve_order` feature
/// is enabled — the golden fixtures encode that order and break loudly if
/// it ever changes.
pub fn bd_record(bead: &ExportBead) -> Result<BdRecord, CoreError> {
    let issue_type = match bead.kind.as_str() {
        "memory" => {
            return Ok(BdRecord::Memory(BdMemory {
                record: "memory",
                key: bead.id.clone(),
                value: bead.title.clone(),
            }));
        }
        "task" => "task",
        "mail" => "message",
        other => {
            return Err(CoreError::Export(format!(
                "bead {} has unknown type {other:?}",
                bead.id
            )));
        }
    };
    let mut metadata = serde_json::Map::new();
    metadata.insert("camp.rig".into(), bead.rig.clone().into());
    if let Some(claimed_by) = &bead.claimed_by {
        metadata.insert("camp.claimed_by".into(), claimed_by.clone().into());
    }
    if let Some(run_id) = &bead.run_id {
        metadata.insert("camp.run_id".into(), run_id.clone().into());
    }
    if let Some(step_id) = &bead.step_id {
        metadata.insert("camp.step_id".into(), step_id.clone().into());
    }
    if let Some(outcome) = &bead.outcome {
        metadata.insert("gc.outcome".into(), outcome.clone().into());
    }
    let dependencies = bead
        .needs
        .iter()
        .map(|needs_id| BdDependency {
            issue_id: bead.id.clone(),
            depends_on_id: needs_id.clone(),
            dep_type: "blocks",
        })
        .collect();
    Ok(BdRecord::Issue(Box::new(BdIssue {
        record: "issue",
        id: bead.id.clone(),
        title: bead.title.clone(),
        description: bead.description.clone(),
        status: bead.status.clone(),
        priority: 2,
        issue_type,
        assignee: bead.assignee.clone(),
        created_at: bead.created_ts.clone(),
        updated_at: bead.updated_ts.clone(),
        closed_at: bead.closed_ts.clone(),
        close_reason: bead.close_reason.clone(),
        labels: bead.labels.clone(),
        dependencies,
        metadata,
    })))
}

/// One `beads.jsonl` line (no trailing newline).
pub fn jsonl_line(record: &BdRecord) -> Result<String, CoreError> {
    Ok(match record {
        BdRecord::Issue(issue) => serde_json::to_string(issue)?,
        BdRecord::Memory(memory) => serde_json::to_string(memory)?,
    })
}

/// A gc `orders/<name>.toml` file: an `[order]` table. gc derives the
/// order's name from the FILENAME, so the name lives beside this struct,
/// not in it. Keys per gascity `internal/orders/order.go` at the pinned
/// ref: `trigger` (required), `schedule` (cron), `on` (event), `formula`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct GcOrderFile {
    pub order: GcOrder,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct GcOrder {
    pub formula: String,
    pub trigger: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on: Option<String>,
}

/// The outcome of translating one camp order (plan decision 8: an explicit
/// mapping table, failing fast on anything gc order TOML cannot express).
#[derive(Debug, Clone, PartialEq)]
pub enum OrderTranslation {
    Translated { name: String, file: GcOrderFile },
    Untranslatable { name: String, reason: String },
}

/// Translate one compiled camp order to gc order TOML. Translation table:
/// docs/reference/export.md. `raw` is the same order's `[[order]]` config,
/// needed because the compiled form defaults `catch_up_window` and would
/// hide whether the camp declared one.
pub fn translate_order(order: &Order, raw: &OrderConfig) -> OrderTranslation {
    let name = order.name.clone();
    if raw.catch_up_window.is_some() {
        return OrderTranslation::Untranslatable {
            name,
            reason: "catch_up_window has no gc order-TOML equivalent".to_owned(),
        };
    }
    if let Some(rig) = &order.rig {
        return OrderTranslation::Untranslatable {
            name,
            reason: format!(
                "rig {rig:?} cannot be expressed in gc order TOML (gc's scope key is city|rig \
                 with no named-rig binding; pack placement picks the rig)"
            ),
        };
    }
    match &order.trigger {
        Trigger::Cron { expr } => OrderTranslation::Translated {
            name,
            file: GcOrderFile {
                order: GcOrder {
                    formula: order.formula.clone(),
                    trigger: "cron",
                    schedule: Some(expr.source().to_owned()),
                    on: None,
                },
            },
        },
        Trigger::Event {
            event_type,
            label: None,
        } => OrderTranslation::Translated {
            name,
            file: GcOrderFile {
                order: GcOrder {
                    formula: order.formula.clone(),
                    trigger: "event",
                    schedule: None,
                    on: Some(event_type.clone()),
                },
            },
        },
        Trigger::Event {
            event_type,
            label: Some(label),
        } => OrderTranslation::Untranslatable {
            name,
            reason: format!(
                "event trigger {event_type:?} has a [label={label}] filter — gc event orders \
                 have no label filter"
            ),
        },
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::clock::FixedClock;
    use crate::event::{EventInput, EventType};
    use crate::ledger::Ledger;

    pub(crate) const TS: &str = "2026-07-05T21:14:03Z";

    pub(crate) fn temp_ledger() -> (tempfile::TempDir, Ledger) {
        let dir = tempfile::tempdir().unwrap();
        let ledger =
            Ledger::open_with_clock(&dir.path().join("camp.db"), Box::new(FixedClock::new(TS)))
                .unwrap();
        (dir, ledger)
    }

    pub(crate) fn append(
        ledger: &mut Ledger,
        kind: EventType,
        bead: &str,
        data: serde_json::Value,
    ) {
        ledger
            .append(EventInput {
                kind,
                rig: Some("gc".into()),
                actor: "test".into(),
                bead: Some(bead.into()),
                data,
            })
            .unwrap();
    }

    /// gc-1 closed with outcome+reason after a claim; gc-2 open with
    /// description/needs/labels/assignee; gc-3 mail; gc-4 memory; gc-5
    /// with run/step provenance.
    pub(crate) fn seed(ledger: &mut Ledger) {
        append(
            ledger,
            EventType::BeadCreated,
            "gc-1",
            serde_json::json!({"title": "implement widget", "labels": ["cli"]}),
        );
        append(
            ledger,
            EventType::BeadClaimed,
            "gc-1",
            serde_json::json!({"session": "camp/dev/1"}),
        );
        append(
            ledger,
            EventType::BeadClosed,
            "gc-1",
            serde_json::json!({"outcome": "pass", "reason": "shipped the widget"}),
        );
        append(
            ledger,
            EventType::BeadCreated,
            "gc-2",
            serde_json::json!({
                "title": "review widget",
                "description": "the change",
                "needs": ["gc-1"],
                "labels": ["cli", "review"],
                "assignee": "reviewer"
            }),
        );
        append(
            ledger,
            EventType::BeadCreated,
            "gc-3",
            serde_json::json!({"title": "ping from ci", "type": "mail"}),
        );
        append(
            ledger,
            EventType::BeadCreated,
            "gc-4",
            serde_json::json!({"title": "deploy needs the VPN profile", "type": "memory"}),
        );
        append(
            ledger,
            EventType::BeadCreated,
            "gc-5",
            serde_json::json!({
                "title": "step one",
                "run_id": "20260705T211403Z-abc123",
                "step_id": "s1"
            }),
        );
    }

    #[test]
    fn export_beads_returns_full_fidelity_rows_in_creation_order() {
        let (_dir, mut ledger) = temp_ledger();
        seed(&mut ledger);

        let beads = ledger.export_beads().unwrap();
        assert_eq!(
            beads.iter().map(|b| b.id.as_str()).collect::<Vec<_>>(),
            vec!["gc-1", "gc-2", "gc-3", "gc-4", "gc-5"]
        );

        let b1 = &beads[0];
        assert_eq!(b1.status, "closed");
        assert_eq!(b1.kind, "task");
        assert_eq!(b1.rig, "gc");
        assert_eq!(b1.claimed_by.as_deref(), Some("camp/dev/1"));
        assert_eq!(b1.outcome.as_deref(), Some("pass"));
        assert_eq!(b1.close_reason.as_deref(), Some("shipped the widget"));
        assert_eq!(b1.closed_ts.as_deref(), Some(TS));
        assert_eq!(b1.labels, vec!["cli".to_owned()]);
        assert_eq!(b1.created_ts, TS);
        assert_eq!(b1.updated_ts, TS);

        let b2 = &beads[1];
        assert_eq!(b2.description, "the change");
        assert_eq!(b2.needs, vec!["gc-1".to_owned()]);
        assert_eq!(b2.assignee.as_deref(), Some("reviewer"));
        assert_eq!(b2.status, "open");
        assert_eq!(b2.outcome, None);
        assert_eq!(b2.closed_ts, None);

        assert_eq!(beads[2].kind, "mail");
        assert_eq!(beads[3].kind, "memory");

        let b5 = &beads[4];
        assert_eq!(b5.run_id.as_deref(), Some("20260705T211403Z-abc123"));
        assert_eq!(b5.step_id.as_deref(), Some("s1"));
    }

    fn full_bead() -> ExportBead {
        ExportBead {
            id: "gc-1".into(),
            rig: "gc".into(),
            kind: "task".into(),
            title: "implement widget".into(),
            description: "the change".into(),
            status: "closed".into(),
            assignee: Some("dev".into()),
            claimed_by: Some("camp/dev/1".into()),
            outcome: Some("pass".into()),
            close_reason: Some("shipped the widget".into()),
            labels: vec!["cli".into()],
            run_id: None,
            step_id: None,
            needs: vec!["gc-0".into()],
            created_ts: TS.into(),
            updated_ts: TS.into(),
            closed_ts: Some(TS.into()),
        }
    }

    fn minimal_bead() -> ExportBead {
        ExportBead {
            id: "gc-2".into(),
            rig: "gc".into(),
            kind: "task".into(),
            title: "review".into(),
            description: String::new(),
            status: "open".into(),
            assignee: None,
            claimed_by: None,
            outcome: None,
            close_reason: None,
            labels: vec![],
            run_id: None,
            step_id: None,
            needs: vec![],
            created_ts: TS.into(),
            updated_ts: TS.into(),
            closed_ts: None,
        }
    }

    #[test]
    fn closed_task_maps_to_a_bd_issue_line_exactly() {
        let line = jsonl_line(&bd_record(&full_bead()).unwrap()).unwrap();
        assert_eq!(
            line,
            r#"{"_type":"issue","id":"gc-1","title":"implement widget","description":"the change","status":"closed","priority":2,"issue_type":"task","assignee":"dev","created_at":"2026-07-05T21:14:03Z","updated_at":"2026-07-05T21:14:03Z","closed_at":"2026-07-05T21:14:03Z","close_reason":"shipped the widget","labels":["cli"],"dependencies":[{"issue_id":"gc-1","depends_on_id":"gc-0","type":"blocks"}],"metadata":{"camp.claimed_by":"camp/dev/1","camp.rig":"gc","gc.outcome":"pass"}}"#
        );
    }

    #[test]
    fn open_minimal_task_omits_empty_fields_and_keeps_priority() {
        let line = jsonl_line(&bd_record(&minimal_bead()).unwrap()).unwrap();
        assert_eq!(
            line,
            r#"{"_type":"issue","id":"gc-2","title":"review","status":"open","priority":2,"issue_type":"task","created_at":"2026-07-05T21:14:03Z","updated_at":"2026-07-05T21:14:03Z","metadata":{"camp.rig":"gc"}}"#
        );
    }

    #[test]
    fn mail_maps_to_the_native_bd_message_type() {
        let mut bead = minimal_bead();
        bead.kind = "mail".into();
        let line = jsonl_line(&bd_record(&bead).unwrap()).unwrap();
        assert!(line.contains(r#""issue_type":"message""#), "{line}");
    }

    #[test]
    fn memory_maps_to_a_native_bd_memory_record() {
        let mut bead = minimal_bead();
        bead.id = "gc-4".into();
        bead.kind = "memory".into();
        bead.title = "deploy needs the VPN profile".into();
        let line = jsonl_line(&bd_record(&bead).unwrap()).unwrap();
        assert_eq!(
            line,
            r#"{"_type":"memory","key":"gc-4","value":"deploy needs the VPN profile"}"#
        );
    }

    #[test]
    fn run_and_step_provenance_ride_in_camp_metadata() {
        let mut bead = minimal_bead();
        bead.run_id = Some("20260705T211403Z-abc123".into());
        bead.step_id = Some("s1".into());
        let line = jsonl_line(&bd_record(&bead).unwrap()).unwrap();
        assert!(
            line.contains(r#""camp.run_id":"20260705T211403Z-abc123""#)
                && line.contains(r#""camp.step_id":"s1""#),
            "{line}"
        );
    }

    #[test]
    fn unknown_bead_type_is_an_export_error() {
        let mut bead = minimal_bead();
        bead.kind = "vibes".into();
        match bd_record(&bead) {
            Err(CoreError::Export(msg)) => assert!(msg.contains("vibes"), "{msg}"),
            other => panic!("expected Export error, got {other:?}"),
        }
    }

    /// Compile a camp.toml text and hand back (compiled, raw) order pairs.
    fn orders_from(
        toml_text: &str,
    ) -> Vec<(crate::orders::Order, crate::orders::parse::OrderConfig)> {
        let config = crate::config::CampConfig::parse(toml_text).unwrap();
        let compiled = crate::orders::parse::compile_orders(&config).unwrap();
        compiled.into_iter().zip(config.orders).collect()
    }

    const RIGGED: &str = r#"
[camp]
name = "golden"

[[rigs]]
name = "gc"
path = "/tmp/rig"
prefix = "gc"
"#;

    #[test]
    fn cron_order_translates_to_trigger_and_schedule() {
        let text = format!(
            "{RIGGED}\n[[order]]\nname = \"nightly\"\non = \"cron:0 7 * * 1-5\"\nformula = \"one-step\"\n"
        );
        let pairs = orders_from(&text);
        match translate_order(&pairs[0].0, &pairs[0].1) {
            OrderTranslation::Translated { name, file } => {
                assert_eq!(name, "nightly");
                assert_eq!(
                    toml::to_string(&file).unwrap(),
                    "[order]\nformula = \"one-step\"\ntrigger = \"cron\"\nschedule = \"0 7 * * 1-5\"\n"
                );
            }
            other => panic!("expected Translated, got {other:?}"),
        }
    }

    #[test]
    fn event_order_translates_to_trigger_and_on() {
        let text = format!(
            "{RIGGED}\n[[order]]\nname = \"on-close\"\non = \"event:bead.closed\"\nformula = \"one-step\"\n"
        );
        let pairs = orders_from(&text);
        match translate_order(&pairs[0].0, &pairs[0].1) {
            OrderTranslation::Translated { file, .. } => assert_eq!(
                toml::to_string(&file).unwrap(),
                "[order]\nformula = \"one-step\"\ntrigger = \"event\"\non = \"bead.closed\"\n"
            ),
            other => panic!("expected Translated, got {other:?}"),
        }
    }

    #[test]
    fn label_filter_rig_and_catch_up_window_are_untranslatable() {
        let text = format!(
            concat!(
                "{}\n",
                "[[order]]\nname = \"ci-red\"\non = \"event:bead.closed[label=ci-red]\"\nformula = \"fix-ci\"\n\n",
                "[[order]]\nname = \"rigged\"\non = \"cron:0 7 * * *\"\nformula = \"one-step\"\nrig = \"gc\"\n\n",
                "[[order]]\nname = \"caught-up\"\non = \"cron:0 7 * * *\"\nformula = \"one-step\"\ncatch_up_window = \"4h\"\n"
            ),
            RIGGED
        );
        let pairs = orders_from(&text);
        let reasons: Vec<(String, String)> = pairs
            .iter()
            .map(|(order, raw)| match translate_order(order, raw) {
                OrderTranslation::Untranslatable { name, reason } => (name, reason),
                other => panic!("expected Untranslatable, got {other:?}"),
            })
            .collect();
        assert_eq!(reasons[0].0, "ci-red");
        assert!(reasons[0].1.contains("label"), "{}", reasons[0].1);
        assert_eq!(reasons[1].0, "rigged");
        assert!(reasons[1].1.contains("rig"), "{}", reasons[1].1);
        assert_eq!(reasons[2].0, "caught-up");
        assert!(reasons[2].1.contains("catch_up_window"), "{}", reasons[2].1);
    }
}

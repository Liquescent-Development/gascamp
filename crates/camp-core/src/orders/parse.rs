//! `[[order]]` tables (spec §9): the raw camp.toml shape and its
//! compilation into `Order`s. TOML-level errors (unknown keys, bad types)
//! surface from serde with line/col via `CampConfig::parse`; every
//! semantic error here names the order and the field.
//!
//! compat §14: `compile_all_orders` also scans materialized imported
//! orders (`<root>/imports/<binding>/orders/*.toml`), namespaced
//! `<binding>.<stem>`. An imported order is INERT until `[orders] enabled`
//! names it — the money invariant.

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::CampConfig;
use crate::error::CoreError;
use crate::event::EventType;
use crate::orders::cron::CronExpr;
use crate::orders::{Order, Trigger};

/// Spec §9: missed fires within this window fire once on wake; `"0"` in
/// camp.toml disables catch-up.
pub const DEFAULT_CATCH_UP_WINDOW: Duration = Duration::from_secs(2 * 60 * 60);

/// The largest accepted catch-up window (PR #13 review LOW 7): the window
/// bounds the synchronous missed-fire scan every startup and jump
/// recompute performs, so an unbounded window is a latency bug waiting in
/// camp.toml. 7 days ≈ 10K scan steps for a minutely order — ample for
/// "laptop was off over a long weekend", rejected loudly beyond that
/// (consistent with the never-firing-cron fail-fast).
pub const MAX_CATCH_UP_WINDOW: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// One raw `[[order]]` table, exactly as spec §9 writes it. Unknown keys
/// are rejected at parse (deny_unknown_fields — a typo never becomes dead
/// config).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderConfig {
    pub name: String,
    pub on: String,
    pub formula: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rig: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catch_up_window: Option<String>,
}

/// Order names become part of event actors (`order:<name>:<fired-seq>`,
/// plan Decision J), so the charset is pinned.
const NAME_PATTERN_DOC: &str = "^[a-z0-9][a-z0-9_-]*$";

fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// Compile every `[[order]]` table into an `Order`, validating names,
/// triggers, rigs, and windows. Every error names the order and the field.
pub fn compile_orders(config: &CampConfig) -> Result<Vec<Order>, CoreError> {
    let mut seen = std::collections::BTreeSet::new();
    let mut orders = Vec::with_capacity(config.orders.len());
    for raw in &config.orders {
        let field_err = |field: &str, reason: String| CoreError::Order {
            order: raw.name.clone(),
            reason: format!("field {field:?}: {reason}"),
        };
        if !valid_name(&raw.name) {
            return Err(CoreError::Order {
                order: raw.name.clone(),
                reason: format!(
                    "field \"name\": must match {NAME_PATTERN_DOC} (it becomes part of event actors)"
                ),
            });
        }
        if !seen.insert(raw.name.clone()) {
            return Err(CoreError::Order {
                order: raw.name.clone(),
                reason: "duplicate order name".into(),
            });
        }
        if raw.formula.is_empty() {
            return Err(field_err("formula", "must not be empty".into()));
        }
        if let Some(rig) = &raw.rig {
            config
                .rig(rig)
                .map_err(|_| field_err("rig", format!("unknown rig {rig:?}")))?;
        }
        let trigger = parse_trigger(&raw.on).map_err(|reason| field_err("on", reason))?;
        let catch_up_window = match raw.catch_up_window.as_deref() {
            None => DEFAULT_CATCH_UP_WINDOW,
            Some("0") => Duration::ZERO,
            Some(text) => {
                parse_window(text).map_err(|reason| field_err("catch_up_window", reason))?
            }
        };
        orders.push(Order {
            name: raw.name.clone(),
            trigger,
            formula: raw.formula.clone(),
            rig: raw.rig.clone(),
            catch_up_window,
        });
    }
    Ok(orders)
}

/// One imported order that `[orders] enabled` has not armed. It is listed
/// (so the operator can see and enable it) but never fires.
#[derive(Debug, Clone, PartialEq)]
pub struct DisabledOrder {
    pub name: String,
    /// The binding the order was imported under (`<binding>.<stem>`).
    pub source: String,
    pub formula: String,
}

/// The full order inventory: armed (`active`) + inert-imported (`disabled`).
/// Only `active` is iterated by the fire loop, so a disabled imported order
/// is unreachable — the money invariant (compat §14).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OrderInventory {
    pub active: Vec<Order>,
    pub disabled: Vec<DisabledOrder>,
}

/// A gc order file as written on disk (`[order]` with `formula`, `trigger`,
/// `schedule`/`on`). Tolerant of extra keys gc may carry.
#[derive(Debug, Clone, Deserialize)]
struct GcOrderFile {
    order: GcOrderRaw,
}

#[derive(Debug, Clone, Deserialize)]
struct GcOrderRaw {
    formula: String,
    trigger: String,
    #[serde(default)]
    schedule: Option<String>,
    #[serde(default)]
    on: Option<String>,
}

/// Build a camp `Order` from an imported gc order file, namespaced
/// `<binding>.<stem>`. Reuses `parse_trigger` by reconstructing the camp
/// `on` string ("cron:<expr>" / "event:<type>") so validation is identical.
fn parse_gc_order(binding: &str, stem: &str, raw: GcOrderRaw) -> Result<Order, CoreError> {
    let name = format!("{binding}.{stem}");
    let on_str = match raw.trigger.as_str() {
        "cron" => {
            let schedule = raw.schedule.ok_or_else(|| CoreError::Order {
                order: name.clone(),
                reason: "field \"schedule\": cron trigger requires a schedule".to_owned(),
            })?;
            format!("cron:{schedule}")
        }
        "event" => {
            let on = raw.on.ok_or_else(|| CoreError::Order {
                order: name.clone(),
                reason: "field \"on\": event trigger requires an on spec".to_owned(),
            })?;
            format!("event:{on}")
        }
        other => {
            return Err(CoreError::Order {
                order: name.clone(),
                reason: format!("field \"trigger\": unknown trigger {other:?} (cron|event)"),
            });
        }
    };
    let trigger = parse_trigger(&on_str).map_err(|reason| CoreError::Order {
        order: name.clone(),
        reason: format!("field \"on\": {reason}"),
    })?;
    Ok(Order {
        name,
        trigger,
        formula: raw.formula,
        rig: None,
        catch_up_window: DEFAULT_CATCH_UP_WINDOW,
    })
}

/// Compile every active order — local `[[order]]` tables plus the imported
/// orders that `[orders] enabled` names — and list the inert imported ones.
/// An imported order whose formula does not resolve is a hard error at load
/// (fail fast, naming the order + formula).
pub fn compile_all_orders(cfg: &CampConfig) -> Result<OrderInventory, CoreError> {
    let mut active = compile_orders(cfg)?;
    let mut disabled = Vec::new();
    let Some(root) = cfg.root.as_deref() else {
        return Ok(OrderInventory { active, disabled });
    };
    let _ = root;
    // Orders come from the DIRECT imports, driven by the `[imports.*]`
    // DECLARATIONS — not by listing `imports/`. A local-path import is layered
    // in place and has no dir there (D7), and the `.transitive` sentinel is
    // not a binding, so listing the directory would both miss orders and
    // invent a binding that no operator declared.
    let mut layers = cfg.import_layers();
    layers.sort();
    for (binding, layer) in layers {
        let orders_dir = layer.join("orders");
        let Ok(order_entries) = std::fs::read_dir(&orders_dir) else {
            continue;
        };
        let mut order_files: Vec<PathBuf> = order_entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "toml"))
            .collect();
        order_files.sort();
        for order_file in order_files {
            let stem = order_file
                .file_stem()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_owned();
            let text = std::fs::read_to_string(&order_file).map_err(|e| CoreError::Order {
                order: format!("{binding}.{stem}"),
                reason: format!("cannot read {}: {e}", order_file.display()),
            })?;
            let file: GcOrderFile = toml::from_str(&text).map_err(|e| CoreError::Order {
                order: format!("{binding}.{stem}"),
                reason: format!("invalid TOML: {e}"),
            })?;
            let order = parse_gc_order(&binding, &stem, file.order)?;
            // Verify the formula resolves before arming — a missing formula
            // is a hard error at load, not a silent fire-time miss.
            crate::orders::resolve_formula(cfg, &order.formula).map_err(|e| CoreError::Order {
                order: order.name.clone(),
                reason: format!("formula {:?}: {e}", order.formula),
            })?;
            if cfg.orders_section.enabled.contains(&order.name) {
                active.push(order);
            } else {
                disabled.push(DisabledOrder {
                    name: order.name,
                    source: binding.clone(),
                    formula: order.formula,
                });
            }
        }
    }
    Ok(OrderInventory { active, disabled })
}

/// `on = "cron:<expr>"` or `on = "event:<type>[label=<value>]"` (spec §9).
fn parse_trigger(on: &str) -> Result<Trigger, String> {
    if let Some(expr) = on.strip_prefix("cron:") {
        return Ok(Trigger::Cron {
            expr: CronExpr::parse(expr)?,
        });
    }
    if let Some(rest) = on.strip_prefix("event:") {
        let (event_type, label) = match rest.split_once('[') {
            None => (rest, None),
            Some((ty, bracket)) => {
                let inner = bracket
                    .strip_suffix(']')
                    .ok_or_else(|| format!("unterminated filter in {rest:?}"))?;
                let value = inner.strip_prefix("label=").ok_or_else(|| {
                    format!("only [label=…] filters are supported, got {inner:?}")
                })?;
                if value.is_empty() {
                    return Err("label filter value must not be empty".into());
                }
                (ty, Some(value.to_owned()))
            }
        };
        EventType::parse(event_type).map_err(|_| format!("unknown event type {event_type:?}"))?;
        if label.is_some() && !event_type.starts_with("bead.") {
            return Err(format!(
                "label filters match beads; {event_type:?} is not a bead.* event"
            ));
        }
        return Ok(Trigger::Event {
            event_type: event_type.to_owned(),
            label,
        });
    }
    Err(format!(
        "expected \"cron:<expr>\" or \"event:<type>[label=<value>]\", got {on:?}"
    ))
}

fn parse_window(text: &str) -> Result<Duration, String> {
    let signed: jiff::SignedDuration = text.parse().map_err(|e| {
        format!(
            "{text:?} is not a duration ({e}); use forms like \"2h\", \"30m\", or \"0\" to disable"
        )
    })?;
    if signed.is_negative() {
        return Err(format!("{text:?} is negative"));
    }
    let window = Duration::try_from(signed).map_err(|e| format!("{text:?}: {e}"))?;
    if window > MAX_CATCH_UP_WINDOW {
        return Err(format!(
            "{text:?} exceeds the {}d maximum catch-up window",
            MAX_CATCH_UP_WINDOW.as_secs() / 86_400
        ));
    }
    Ok(window)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn camp_with_imported_order(enabled: &[&str]) -> (tempfile::TempDir, CampConfig) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut toml = String::from(
            "[camp]\nname=\"t\"\n\n[[rigs]]\nname=\"gc\"\npath=\"/p\"\nprefix=\"gc\"\n\n[imports.bmad]\nsource=\"file:///x\"\n",
        );
        if !enabled.is_empty() {
            toml.push_str(&format!(
                "\n[orders]\nenabled = [{}]\n",
                enabled
                    .iter()
                    .map(|e| format!("\"{e}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        std::fs::write(root.join("camp.toml"), &toml).unwrap();
        let od = root.join("imports/bmad/orders");
        std::fs::create_dir_all(&od).unwrap();
        std::fs::write(od.join("nightly.toml"), "[order]\nformula = \"nightly-formula\"\ntrigger = \"cron\"\nschedule = \"0 2 * * *\"\n").unwrap();
        let fd = root.join("imports/bmad/formulas");
        std::fs::create_dir_all(&fd).unwrap();
        std::fs::write(
            fd.join("nightly-formula.toml"),
            "formula = \"nightly-formula\"\n",
        )
        .unwrap();
        let cfg = CampConfig::load(&root.join("camp.toml")).unwrap();
        (dir, cfg)
    }

    fn one_order_cfg(body: &str) -> CampConfig {
        CampConfig::parse(&format!(
            "[camp]\nname=\"d\"\n\n[[rigs]]\nname=\"gc\"\npath=\"/p\"\nprefix=\"gc\"\n\n[[order]]\n{body}\n"
        ))
        .unwrap()
    }

    #[test]
    fn compiles_the_spec_section_9_example() {
        let cfg = CampConfig::parse(
            r#"
[camp]
name = "dev"

[[rigs]]
name = "gascity"
path = "/code/gascity"
prefix = "gc"

[[order]]
name    = "morning-triage"
on      = "cron:0 7 * * 1-5"
formula = "triage-inbox"
rig     = "gascity"

[[order]]
name    = "ci-red"
on      = "event:bead.closed[label=ci-red]"
formula = "fix-ci"
"#,
        )
        .unwrap();
        let orders = compile_orders(&cfg).unwrap();
        assert_eq!(orders.len(), 2);
        assert_eq!(orders[0].name, "morning-triage");
        assert!(
            matches!(&orders[0].trigger, Trigger::Cron { expr } if expr.source() == "0 7 * * 1-5")
        );
        assert_eq!(orders[0].rig.as_deref(), Some("gascity"));
        assert_eq!(orders[0].catch_up_window, DEFAULT_CATCH_UP_WINDOW);
        assert!(matches!(&orders[1].trigger,
            Trigger::Event { event_type, label }
            if event_type == "bead.closed" && label.as_deref() == Some("ci-red")));
    }

    #[test]
    fn window_parses_friendly_durations_and_zero_disables() {
        for (text, expect) in [
            ("30m", Duration::from_secs(30 * 60)),
            ("2h", Duration::from_secs(2 * 60 * 60)),
            ("168h", MAX_CATCH_UP_WINDOW), // exactly the 7d cap: accepted
            ("0", Duration::ZERO),
        ] {
            let cfg = one_order_cfg(&format!(
                "name=\"n\"\non=\"cron:0 7 * * *\"\nformula=\"f\"\ncatch_up_window=\"{text}\""
            ));
            assert_eq!(
                compile_orders(&cfg).unwrap()[0].catch_up_window,
                expect,
                "{text}"
            );
        }
    }

    #[test]
    fn errors_name_the_order_and_the_field() {
        for (body, hits) in [
            ("name=\"x\"\non=\"daily\"\nformula=\"f\"", vec!["x", "on"]),
            (
                "name=\"x\"\non=\"cron:61 * * * *\"\nformula=\"f\"",
                vec!["x", "on", "minute"],
            ),
            (
                "name=\"x\"\non=\"event:bogus.event\"\nformula=\"f\"",
                vec!["x", "on", "bogus.event"],
            ),
            (
                "name=\"x\"\non=\"event:campd.started[label=y]\"\nformula=\"f\"",
                vec!["x", "on", "label"],
            ),
            (
                "name=\"x\"\non=\"event:bead.closed[label=]\"\nformula=\"f\"",
                vec!["x", "on", "label"],
            ),
            (
                "name=\"x\"\non=\"event:bead.closed[color=red]\"\nformula=\"f\"",
                vec!["x", "on"],
            ),
            (
                "name=\"x\"\non=\"cron:0 7 * * *\"\nformula=\"f\"\ncatch_up_window=\"soon\"",
                vec!["x", "catch_up_window"],
            ),
            // PR #13 review LOW 7: an unbounded window turns every startup
            // or jump recompute into an unbounded synchronous scan.
            (
                "name=\"x\"\non=\"cron:0 7 * * *\"\nformula=\"f\"\ncatch_up_window=\"8760h\"",
                vec!["x", "catch_up_window", "maximum"],
            ),
            (
                "name=\"x\"\non=\"cron:0 7 * * *\"\nformula=\"f\"\nrig=\"nope\"",
                vec!["x", "rig"],
            ),
            (
                "name=\"Bad Name\"\non=\"cron:0 7 * * *\"\nformula=\"f\"",
                vec!["name"],
            ),
            (
                "name=\"x\"\non=\"cron:0 7 * * *\"\nformula=\"\"",
                vec!["x", "formula"],
            ),
        ] {
            let cfg = one_order_cfg(body);
            let err = compile_orders(&cfg).unwrap_err().to_string();
            for hit in hits {
                assert!(err.contains(hit), "error {err:?} must contain {hit:?}");
            }
        }
    }

    #[test]
    fn duplicate_order_names_are_rejected() {
        let cfg = CampConfig::parse(
            "[camp]\nname=\"d\"\n\
             [[order]]\nname=\"x\"\non=\"cron:0 7 * * *\"\nformula=\"f\"\n\
             [[order]]\nname=\"x\"\non=\"cron:0 8 * * *\"\nformula=\"g\"\n",
        )
        .unwrap();
        assert!(
            compile_orders(&cfg)
                .unwrap_err()
                .to_string()
                .contains("duplicate")
        );
    }

    #[test]
    fn unknown_order_table_key_is_rejected_at_toml_level() {
        // deny_unknown_fields: the toml error carries line/col — the
        // TOML-syntax layer of "parse errors name the order and the field".
        assert!(
            CampConfig::parse(
                "[camp]\nname=\"d\"\n[[order]]\nname=\"x\"\non=\"cron:0 7 * * *\"\nformula=\"f\"\nbogus=1\n"
            )
            .is_err()
        );
    }

    #[test]
    fn a_negative_window_is_rejected() {
        let cfg = one_order_cfg(
            "name=\"x\"\non=\"cron:0 7 * * *\"\nformula=\"f\"\ncatch_up_window=\"-2h\"",
        );
        let err = compile_orders(&cfg).unwrap_err().to_string();
        assert!(err.contains("negative"), "{err}");
    }

    // ---- compat §14: imported orders + the money invariant -------------

    #[test]
    fn imported_order_is_inert_until_enabled() {
        let (_d, cfg) = camp_with_imported_order(&[]);
        let inv = compile_all_orders(&cfg).unwrap();
        assert!(
            inv.active.iter().all(|o| o.name != "bmad.nightly"),
            "unenabled → NOT active"
        );
        assert!(
            inv.disabled
                .iter()
                .any(|d| d.name == "bmad.nightly" && d.source == "bmad"),
            "disabled with source: {inv:?}"
        );
    }

    #[test]
    fn enabling_arms_exactly_the_named_import_order() {
        let (_d, cfg) = camp_with_imported_order(&["bmad.nightly"]);
        let inv = compile_all_orders(&cfg).unwrap();
        assert!(inv.active.iter().any(|o| o.name == "bmad.nightly"));
        assert!(inv.disabled.iter().all(|d| d.name != "bmad.nightly"));
    }

    #[test]
    fn namespaced_imported_order_name_is_accepted() {
        // The namespaced name `<binding>.<stem>` contains a `.` that
        // `valid_name` rejects; imported names are constructed from the
        // binding + stem directly, NOT run through `valid_name` (the
        // `.`-in-event-actor charset is a phase-2 concern).
        let (_d, cfg) = camp_with_imported_order(&[]);
        let inv = compile_all_orders(&cfg).unwrap();
        assert!(
            inv.disabled.iter().any(|d| d.name == "bmad.nightly"),
            "namespaced imported order name must be accepted: {inv:?}"
        );
    }
}

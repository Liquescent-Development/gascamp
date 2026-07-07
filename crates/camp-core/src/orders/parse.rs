//! `[[order]]` tables (spec §9): the raw camp.toml shape and its
//! compilation into `Order`s. TOML-level errors (unknown keys, bad types)
//! surface from serde with line/col via `CampConfig::parse`; every
//! semantic error here names the order and the field.

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
    Duration::try_from(signed).map_err(|e| format!("{text:?}: {e}"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

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
}

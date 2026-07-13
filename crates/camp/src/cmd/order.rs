//! `camp order ls` / `camp order run <name>` (spec §9). `run` appends the
//! `order.fired` declaration and pokes campd — the daemon cooks it, so a
//! manual fire exercises literally the away-mode pipeline (Phase 10 plan
//! Decision D). compat §14 adds `enable`/`disable` to arm imported orders
//! (the money invariant) and a source + disabled column to `ls`.

use anyhow::{Context, Result, bail};
use camp_core::config::CampConfig;
use camp_core::ledger::Ledger;
use camp_core::orders::parse::compile_all_orders;
use camp_core::orders::parse::compile_orders;
use camp_core::orders::{FireCause, Order, Trigger, fired_input};

use crate::campdir::CampDir;

fn load_orders(camp: &CampDir) -> Result<Vec<Order>> {
    let config = CampConfig::load(&camp.config_path())?;
    Ok(compile_orders(&config)?)
}

/// The raw trigger string for display (inverse of trigger parsing).
fn on_text(order: &Order) -> String {
    match &order.trigger {
        Trigger::Cron { expr } => format!("cron:{}", expr.source()),
        Trigger::Event {
            event_type,
            label: None,
        } => format!("event:{event_type}"),
        Trigger::Event {
            event_type,
            label: Some(label),
        } => format!("event:{event_type}[label={label}]"),
    }
}

/// Next fire in the system timezone, `None` for event orders, `"never"`
/// rendered by the caller for a cron expression off its horizon.
fn next_fire(order: &Order, now: jiff::Timestamp, tz: &jiff::tz::TimeZone) -> Option<String> {
    match &order.trigger {
        Trigger::Cron { expr } => Some(
            expr.next_after(now, tz)
                .map(|ts| ts.to_zoned(tz.clone()).to_string())
                .unwrap_or_else(|| "never".to_owned()),
        ),
        Trigger::Event { .. } => None,
    }
}

pub fn ls(camp: &CampDir, json: bool) -> Result<()> {
    let config = CampConfig::load(&camp.config_path())?;
    let inv = compile_all_orders(&config)?;
    let now = jiff::Timestamp::now();
    let tz = jiff::tz::TimeZone::system();
    let active = inv.active.clone();
    if json {
        let mut rows: Vec<serde_json::Value> = active
            .iter()
            .map(|order| {
                serde_json::json!({
                    "name": order.name,
                    "on": on_text(order),
                    "formula": order.formula,
                    "rig": order.rig,
                    "catch_up_window_secs": order.catch_up_window.as_secs(),
                    "next_fire": next_fire(order, now, &tz),
                    "state": "active",
                })
            })
            .collect();
        for d in &inv.disabled {
            rows.push(serde_json::json!({
                "name": d.name,
                "formula": d.formula,
                "source": d.source,
                "state": "disabled",
            }));
        }
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if active.is_empty() && inv.disabled.is_empty() {
        println!("no orders configured (add [[order]] tables, or `camp import add` a pack with orders/)");
        return Ok(());
    }
    println!(
        "{:<24} {:<10} {:<40} {:<14} {:<8}",
        "NAME", "STATE", "ON", "FORMULA", "SOURCE"
    );
    for order in &active {
        println!(
            "{:<24} {:<10} {:<40} {:<14} {:<8}",
            order.name,
            "active",
            on_text(order),
            order.formula,
            order.rig.as_deref().unwrap_or("-"),
        );
    }
    for d in &inv.disabled {
        println!(
            "{:<24} {:<10} {:<40} {:<14} {:<8}",
            d.name, "disabled", "(imported)", d.formula, d.source,
        );
    }
    Ok(())
}

/// `camp order enable <name>` — arm an imported order by adding it to
/// `[orders] enabled` (surgical camp.toml edit; dedupes).
pub fn enable_order(camp_root: &std::path::Path, name: &str) -> Result<()> {
    let camp_toml = camp_root.join("camp.toml");
    let cfg = CampConfig::load(&camp_toml)?;
    let mut enabled = cfg.orders_section.enabled.clone();
    if !enabled.iter().any(|n| n == name) {
        enabled.push(name.to_owned());
    }
    rewrite_orders_block(&camp_toml, &enabled)?;
    println!("enabled order {name}");
    Ok(())
}

/// `camp order disable <name>` — disarm an imported order by removing it
/// from `[orders] enabled` (surgical camp.toml edit).
pub fn disable_order(camp_root: &std::path::Path, name: &str) -> Result<()> {
    let camp_toml = camp_root.join("camp.toml");
    let cfg = CampConfig::load(&camp_toml)?;
    let enabled: Vec<String> = cfg
        .orders_section
        .enabled
        .iter()
        .filter(|n| n.as_str() != name)
        .cloned()
        .collect();
    rewrite_orders_block(&camp_toml, &enabled)?;
    println!("disabled order {name}");
    Ok(())
}

/// Surgically replace the `[orders]` block in camp.toml (or append one),
/// preserving the rest of the file. An empty `enabled` omits the block.
fn rewrite_orders_block(camp_toml: &std::path::Path, enabled: &[String]) -> Result<()> {
    let text = std::fs::read_to_string(camp_toml)
        .with_context(|| format!("cannot read {}", camp_toml.display()))?;
    let new_block = if enabled.is_empty() {
        String::new()
    } else {
        let list: Vec<String> = enabled.iter().map(|e| format!("\"{e}\"")).collect();
        format!("[orders]\nenabled = [{}]\n", list.join(", "))
    };
    let header = "[orders]";
    let mut out = String::new();
    let mut in_orders = false;
    let mut replaced = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed == header {
            in_orders = true;
            if !replaced {
                if !new_block.is_empty() {
                    out.push_str(&new_block);
                }
                replaced = true;
            }
            continue;
        }
        if in_orders {
            if trimmed.starts_with('[') {
                in_orders = false;
                out.push_str(line);
                out.push('\n');
            }
            // else: part of the orders block being replaced — skip
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if !replaced && !new_block.is_empty() {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
        out.push_str(&new_block);
    }
    std::fs::write(camp_toml, out).with_context(|| format!("cannot write {}", camp_toml.display()))?;
    Ok(())
}

pub fn run_order(camp: &CampDir, name: &str) -> Result<()> {
    let orders = load_orders(camp)?;
    let Some(order) = orders.iter().find(|o| o.name == name) else {
        let names: Vec<&str> = orders.iter().map(|o| o.name.as_str()).collect();
        bail!(
            "no order named {name:?}; configured orders: {}",
            if names.is_empty() {
                "(none)".to_owned()
            } else {
                names.join(", ")
            }
        );
    };
    let mut ledger = Ledger::open(&camp.db_path())?;
    let seq = ledger.append(fired_input(&order.name, &FireCause::Manual))?;
    crate::daemon::socket::poke_best_effort(camp, seq);
    println!(
        "fired order {} (seq {seq}); campd cooks and dispatches it",
        order.name
    );
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod compat_tests {
    use super::*;

    #[test]
    fn enable_adds_and_disable_removes_the_name() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("camp.toml"), "[camp]\nname=\"t\"\n[imports.bmad]\nsource=\"file:///x\"\n").unwrap();
        enable_order(dir.path(), "bmad.nightly").unwrap();
        let cfg = CampConfig::load(&dir.path().join("camp.toml")).unwrap();
        assert!(cfg.orders_section.enabled.contains(&"bmad.nightly".to_string()));
        disable_order(dir.path(), "bmad.nightly").unwrap();
        let cfg = CampConfig::load(&dir.path().join("camp.toml")).unwrap();
        assert!(!cfg.orders_section.enabled.contains(&"bmad.nightly".to_string()));
    }
}

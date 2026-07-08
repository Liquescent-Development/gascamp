//! `camp order ls` / `camp order run <name>` (spec §9). `run` appends the
//! `order.fired` declaration and pokes campd — the daemon cooks it, so a
//! manual fire exercises literally the away-mode pipeline (Phase 10 plan
//! Decision D).

use anyhow::{Result, bail};
use camp_core::config::CampConfig;
use camp_core::ledger::Ledger;
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
    let orders = load_orders(camp)?;
    let now = jiff::Timestamp::now();
    let tz = jiff::tz::TimeZone::system();
    if json {
        let rows: Vec<serde_json::Value> = orders
            .iter()
            .map(|order| {
                serde_json::json!({
                    "name": order.name,
                    "on": on_text(order),
                    "formula": order.formula,
                    "rig": order.rig,
                    "catch_up_window_secs": order.catch_up_window.as_secs(),
                    "next_fire": next_fire(order, now, &tz),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if orders.is_empty() {
        println!("no orders configured (add [[order]] tables to camp.toml)");
        return Ok(());
    }
    println!(
        "{:<16} {:<40} {:<16} {:<8} {:<8} NEXT",
        "NAME", "ON", "FORMULA", "RIG", "WINDOW"
    );
    for order in &orders {
        println!(
            "{:<16} {:<40} {:<16} {:<8} {:<8} {}",
            order.name,
            on_text(order),
            order.formula,
            order.rig.as_deref().unwrap_or("-"),
            format_window(order.catch_up_window),
            next_fire(order, now, &tz).unwrap_or_else(|| "-".to_owned()),
        );
    }
    Ok(())
}

fn format_window(window: std::time::Duration) -> String {
    if window.is_zero() {
        "off".to_owned()
    } else {
        let secs = window.as_secs();
        if secs.is_multiple_of(3600) {
            format!("{}h", secs / 3600)
        } else if secs.is_multiple_of(60) {
            format!("{}m", secs / 60)
        } else {
            format!("{secs}s")
        }
    }
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
    crate::daemon::socket::poke_best_effort(&camp.socket_path(), seq);
    println!(
        "fired order {} (seq {seq}); campd cooks and dispatches it",
        order.name
    );
    Ok(())
}

//! `camp order ls` / `camp order run <name>` (spec §9). `run` appends the
//! `order.fired` declaration and pokes campd — the daemon cooks it, so a
//! manual fire exercises literally the away-mode pipeline (Phase 10 plan
//! Decision D). compat §14 adds `enable`/`disable` to arm imported orders
//! (the money invariant) and a source + disabled column to `ls`. `ls` and
//! `run` both resolve the ACTIVE inventory (local `[[order]]` tables plus
//! the enabled imported pack orders), so an enabled imported order runs
//! and fires exactly like a local one.

use anyhow::{Context, Result, bail};
use camp_core::config::CampConfig;
use camp_core::ledger::Ledger;
use camp_core::orders::parse::compile_all_orders;
use camp_core::orders::{FireCause, Order, Trigger, fired_input};

use crate::campdir::CampDir;

fn load_orders(camp: &CampDir) -> Result<Vec<Order>> {
    let config = CampConfig::load(&camp.config_path())?;
    // ACTIVE orders only — local `[[order]]` tables plus the imported pack
    // orders `[orders] enabled` names (the money invariant). Disabled
    // imported orders are inert and must not resolve for `camp order run`.
    Ok(compile_all_orders(&config)?.active)
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
                // The reason camp cannot run it (a Gas City feature outside
                // camp's cron/event subset), or null for a normal armable one.
                "unsupported": d.unsupported,
            }));
        }
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if active.is_empty() && inv.disabled.is_empty() {
        println!(
            "no orders configured (add [[order]] tables, or `camp import add` a pack with orders/)"
        );
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
        // An order camp cannot run (a Gas City feature outside camp's
        // cron/event subset) is shown as `disabled` with the reason in the ON
        // column — visible and honest, but plainly unarmable.
        let on = match &d.unsupported {
            None => "(imported)".to_owned(),
            Some(reason) => format!("(unsupported: {reason})"),
        };
        println!(
            "{:<24} {:<10} {:<40} {:<14} {:<8}",
            d.name, "disabled", on, d.formula, d.source,
        );
    }
    Ok(())
}

/// `camp order enable <name>` — arm an imported order by adding it to
/// `[orders] enabled` (surgical camp.toml edit; dedupes).
pub fn enable_order(camp_root: &std::path::Path, name: &str) -> Result<()> {
    let camp_toml = camp_root.join("camp.toml");
    let cfg = CampConfig::load(&camp_toml)?;

    // The name must actually NAME an order. Writing an unknown one into
    // `[orders] enabled` printed success and exited 0 while arming nothing —
    // so a typo in the one list that arms money-spending work looked like it
    // had taken effect. Validate against the real inventory and say what exists.
    let inv = camp_core::orders::parse::compile_all_orders(&cfg)?;
    let known: Vec<String> = inv
        .disabled
        .iter()
        .map(|d| d.name.clone())
        .chain(inv.active.iter().map(|o| o.name.clone()))
        .collect();
    if !known.iter().any(|n| n == name) {
        if known.is_empty() {
            bail!(
                "no order named {name:?} — this camp has no orders (import a pack that ships \
                 orders/, or add an [[order]] table)"
            );
        }
        bail!("no order named {name:?} — available orders: {known:?}");
    }

    // Refuse to arm an order camp cannot run: enabling it would make the next
    // `compile_all_orders` (campd startup, export) a hard error — a fail-fast
    // brick. Say so at the moment of intent, naming the reason.
    if let Some(reason) = inv
        .disabled
        .iter()
        .find(|d| d.name == name)
        .and_then(|d| d.unsupported.as_deref())
    {
        bail!(
            "cannot enable order {name:?}: camp cannot run it ({reason}). It uses a Gas City \
             feature outside camp's cron/event order subset — arming it would refuse campd \
             startup. Run it in Gas City, or remove it from the pack."
        );
    }

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
    std::fs::write(camp_toml, out)
        .with_context(|| format!("cannot write {}", camp_toml.display()))?;
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

    /// A camp with a `bmad` import that really ships `orders/nightly.toml` and
    /// the formula it names — so `bmad.nightly` is a REAL order, not a name the
    /// test merely hoped would be accepted.
    fn camp_with_an_imported_order(root: &std::path::Path) {
        std::fs::write(
            root.join("camp.toml"),
            "[camp]\nname=\"t\"\n[imports.bmad]\nsource=\"file:///x\"\n",
        )
        .unwrap();
        let pack = root.join("imports/bmad");
        std::fs::create_dir_all(pack.join("orders")).unwrap();
        std::fs::create_dir_all(pack.join("formulas")).unwrap();
        std::fs::write(
            pack.join("orders/nightly.toml"),
            "[order]\nformula = \"nightly-formula\"\ntrigger = \"cron\"\nschedule = \"0 2 * * *\"\n",
        )
        .unwrap();
        std::fs::write(
            pack.join("formulas/nightly-formula.toml"),
            "formula = \"nightly-formula\"\n",
        )
        .unwrap();
    }

    /// `enable` must refuse a name that is not an order. Writing an unknown name
    /// into `[orders] enabled` printed success and exited 0 while arming
    /// nothing — a typo in the one list that arms money-spending work looked
    /// like it had taken effect.
    #[test]
    fn enabling_an_order_that_does_not_exist_is_refused_and_names_what_does() {
        let dir = tempfile::tempdir().unwrap();
        camp_with_an_imported_order(dir.path());

        let err = enable_order(dir.path(), "bmad.does-not-exist")
            .unwrap_err()
            .to_string();
        assert!(err.contains("no order named"), "got {err}");
        assert!(
            err.contains("bmad.nightly"),
            "the error must name the orders that DO exist, got {err}"
        );

        let cfg = CampConfig::load(&dir.path().join("camp.toml")).unwrap();
        assert!(
            cfg.orders_section.enabled.is_empty(),
            "a refused enable must not have written the name"
        );
    }

    /// #117: after enabling an imported order, `run_order` must RESOLVE it
    /// and append its `order.fired` declaration. Pre-fix `load_orders`
    /// compiled inline `[[order]]` tables only, so `run_order` bailed with
    /// "no order named bmad.nightly" even once enabled.
    #[test]
    fn run_order_resolves_an_enabled_imported_order() {
        let dir = tempfile::tempdir().unwrap();
        camp_with_an_imported_order(dir.path());
        enable_order(dir.path(), "bmad.nightly").unwrap();
        // A ledger must exist at <root>/camp.db for run_order to append to.
        Ledger::open(&dir.path().join("camp.db")).unwrap();
        let camp = CampDir {
            root: dir.path().to_path_buf(),
        };
        run_order(&camp, "bmad.nightly").unwrap();
        // the fire was declared for the imported order
        let ledger = Ledger::open(&dir.path().join("camp.db")).unwrap();
        let fired = ledger
            .events_of_type(camp_core::event::EventType::OrderFired)
            .unwrap();
        assert_eq!(fired.len(), 1, "run_order must declare exactly one fire");
        assert_eq!(fired[0].data["order"], "bmad.nightly");
    }

    /// #117 money invariant on the `run` surface: a DISABLED imported order
    /// must NOT resolve for `camp order run` — it stays inert until enabled.
    /// `load_orders` returns only the ACTIVE inventory, so the disabled name
    /// is unknown and no `order.fired` is ever declared.
    #[test]
    fn run_order_refuses_a_disabled_imported_order() {
        let dir = tempfile::tempdir().unwrap();
        camp_with_an_imported_order(dir.path()); // imported but NOT enabled
        Ledger::open(&dir.path().join("camp.db")).unwrap();
        let camp = CampDir {
            root: dir.path().to_path_buf(),
        };
        let err = run_order(&camp, "bmad.nightly").unwrap_err().to_string();
        assert!(
            err.contains("no order named"),
            "a disabled imported order must not resolve: {err}"
        );
        // and nothing was fired — the money invariant, not just the message
        let ledger = Ledger::open(&dir.path().join("camp.db")).unwrap();
        assert!(
            ledger
                .events_of_type(camp_core::event::EventType::OrderFired)
                .unwrap()
                .is_empty(),
            "no fire may be declared for a disabled imported order"
        );
    }

    /// compat §14 money invariant: `camp order enable` must REFUSE an order
    /// camp cannot run (a Gas City trigger outside camp's cron/event subset),
    /// naming the reason — arming it would brick campd startup. Nothing is
    /// written to `[orders] enabled` on the refusal.
    #[test]
    fn enable_refuses_an_unsupported_imported_order() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("camp.toml"),
            "[camp]\nname=\"t\"\n[imports.bmad]\nsource=\"file:///x\"\n",
        )
        .unwrap();
        let od = dir.path().join("imports/bmad/orders");
        std::fs::create_dir_all(&od).unwrap();
        std::fs::write(
            od.join("cooldown.toml"),
            "[order]\nformula = \"digest\"\ntrigger = \"cooldown\"\ninterval = \"24h\"\n",
        )
        .unwrap();
        let err = enable_order(dir.path(), "bmad.cooldown")
            .unwrap_err()
            .to_string();
        assert!(err.contains("cannot enable"), "must refuse: {err}");
        assert!(err.contains("cooldown"), "must name the reason: {err}");
        let cfg = CampConfig::load(&dir.path().join("camp.toml")).unwrap();
        assert!(
            cfg.orders_section.enabled.is_empty(),
            "a refused enable must write nothing to [orders] enabled"
        );
    }

    #[test]
    fn enable_adds_and_disable_removes_the_name() {
        let dir = tempfile::tempdir().unwrap();
        camp_with_an_imported_order(dir.path());
        enable_order(dir.path(), "bmad.nightly").unwrap();
        let cfg = CampConfig::load(&dir.path().join("camp.toml")).unwrap();
        assert!(
            cfg.orders_section
                .enabled
                .contains(&"bmad.nightly".to_string())
        );
        disable_order(dir.path(), "bmad.nightly").unwrap();
        let cfg = CampConfig::load(&dir.path().join("camp.toml")).unwrap();
        assert!(
            !cfg.orders_section
                .enabled
                .contains(&"bmad.nightly".to_string())
        );
    }
}

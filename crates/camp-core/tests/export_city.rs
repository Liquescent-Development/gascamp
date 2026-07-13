#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Phase 14: export_city orchestration behavior (spec §15.3). The golden
//! byte-level test lives in export_golden.rs; this file pins the rules:
//! fail-before-write on untranslatable orders, the explicit skip, the
//! non-empty-dir refusal, the runs/ archive rules, and read-only-ness.

use std::path::{Path, PathBuf};

use camp_core::clock::FixedClock;
use camp_core::config::CampConfig;
use camp_core::error::CoreError;
use camp_core::event::{EventInput, EventType};
use camp_core::export::{ExportOptions, export_city};
use camp_core::ledger::Ledger;

const TS: &str = "2026-07-05T21:14:03Z";

const NO_SKIP: ExportOptions = ExportOptions {
    skip_untranslatable: false,
};
const SKIP: ExportOptions = ExportOptions {
    skip_untranslatable: true,
};

/// A camp root with a ledger (one closed bead, one memory), an authored
/// formula, an agents dir, and the given [[order]] tables.
fn fixture_camp(dir: &Path, orders_toml: &str) -> (PathBuf, Ledger, CampConfig) {
    let camp_root = dir.join(".camp");
    std::fs::create_dir_all(&camp_root).unwrap();
    let config_text = format!(
        "[camp]\nname = \"golden\"\n\n[[rigs]]\nname = \"gc\"\npath = {:?}\nprefix = \"gc\"\n{orders_toml}",
        dir.join("repo").display()
    );
    std::fs::write(camp_root.join("camp.toml"), &config_text).unwrap();
    // `load` (not `parse`) so config.root is set — export's formula copy goes
    // through `resolve_formula`, which needs the root (compat Task 13).
    let config = CampConfig::load(&camp_root.join("camp.toml")).unwrap();

    std::fs::create_dir_all(camp_root.join("formulas")).unwrap();
    std::fs::write(
        camp_root.join("formulas/one-step.toml"),
        "formula = \"one-step\"\n\n[[steps]]\nid = \"s1\"\ntitle = \"one step\"\n",
    )
    .unwrap();
    // An agent is a DIRECTORY (compat §5.1): agents/dev/ with agent.toml + prompt.md.
    std::fs::create_dir_all(camp_root.join("agents/dev")).unwrap();
    std::fs::write(
        camp_root.join("agents/dev/agent.toml"),
        "description = \"dev\"\n",
    )
    .unwrap();
    std::fs::write(camp_root.join("agents/dev/prompt.md"), "# dev agent\n").unwrap();

    let mut ledger =
        Ledger::open_with_clock(&camp_root.join("camp.db"), Box::new(FixedClock::new(TS))).unwrap();
    for (bead, data) in [
        ("gc-1", serde_json::json!({"title": "implement widget"})),
        (
            "gc-2",
            serde_json::json!({"title": "deploy needs VPN", "type": "memory"}),
        ),
    ] {
        ledger
            .append(EventInput {
                kind: EventType::BeadCreated,
                rig: Some("gc".into()),
                actor: "test".into(),
                bead: Some(bead.into()),
                data,
            })
            .unwrap();
    }
    ledger
        .append(EventInput {
            kind: EventType::BeadClosed,
            rig: Some("gc".into()),
            actor: "test".into(),
            bead: Some("gc-1".into()),
            data: serde_json::json!({"outcome": "pass", "reason": "done"}),
        })
        .unwrap();
    (camp_root, ledger, config)
}

const TRANSLATABLE_ORDERS: &str = r#"
[[order]]
name = "nightly"
on = "cron:0 7 * * 1-5"
formula = "one-step"

[[order]]
name = "on-close"
on = "event:bead.closed"
formula = "one-step"
"#;

const MIXED_ORDERS: &str = r#"
[[order]]
name = "nightly"
on = "cron:0 7 * * 1-5"
formula = "one-step"

[[order]]
name = "ci-red"
on = "event:bead.closed[label=ci-red]"
formula = "one-step"

[[order]]
name = "rigged"
on = "cron:0 8 * * *"
formula = "one-step"
rig = "gc"
"#;

#[test]
fn untranslatable_orders_fail_listing_every_one_and_write_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (camp_root, ledger, config) = fixture_camp(dir.path(), MIXED_ORDERS);
    let out = dir.path().join("city");
    match export_city(&ledger, &config, &camp_root, &out, &NO_SKIP) {
        Err(CoreError::UntranslatableOrders { count, details }) => {
            assert_eq!(count, 2);
            assert!(
                details.contains("ci-red") && details.contains("label"),
                "{details}"
            );
            assert!(
                details.contains("rigged") && details.contains("rig"),
                "{details}"
            );
        }
        other => panic!("expected UntranslatableOrders, got {other:?}"),
    }
    // fail-before-write: the output dir exists but holds nothing
    assert_eq!(std::fs::read_dir(&out).unwrap().count(), 0);
}

#[test]
fn skip_untranslatable_exports_without_the_offenders() {
    let dir = tempfile::tempdir().unwrap();
    let (camp_root, ledger, config) = fixture_camp(dir.path(), MIXED_ORDERS);
    let out = dir.path().join("city");
    let report = export_city(&ledger, &config, &camp_root, &out, &SKIP).unwrap();
    assert_eq!(report.orders, 1);
    let skipped: Vec<&str> = report
        .skipped_orders
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(skipped, vec!["ci-red", "rigged"]);
    assert!(out.join("pack/orders/nightly.toml").exists());
    assert!(!out.join("pack/orders/ci-red.toml").exists());
    assert!(!out.join("pack/orders/rigged.toml").exists());
}

#[test]
fn a_non_empty_output_directory_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (camp_root, ledger, config) = fixture_camp(dir.path(), TRANSLATABLE_ORDERS);
    let out = dir.path().join("city");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("existing.txt"), "hello").unwrap();
    match export_city(&ledger, &config, &camp_root, &out, &NO_SKIP) {
        Err(CoreError::Export(msg)) => assert!(msg.contains("non-empty"), "{msg}"),
        other => panic!("expected Export error, got {other:?}"),
    }
}

#[test]
fn exported_pack_carries_manifest_agents_orders_and_their_formulas() {
    let dir = tempfile::tempdir().unwrap();
    let (camp_root, ledger, config) = fixture_camp(dir.path(), TRANSLATABLE_ORDERS);
    let out = dir.path().join("city");
    let report = export_city(&ledger, &config, &camp_root, &out, &NO_SKIP).unwrap();

    assert_eq!(
        std::fs::read_to_string(out.join("pack/pack.toml")).unwrap(),
        "[pack]\nname = \"golden\"\nschema = 2\ndescription = \"Exported from gas-camp camp golden\"\n"
    );
    assert_eq!(
        std::fs::read_to_string(out.join("pack/agents/dev/prompt.md")).unwrap(),
        "# dev agent\n"
    );
    assert_eq!(
        std::fs::read_to_string(out.join("pack/orders/nightly.toml")).unwrap(),
        "[order]\nformula = \"one-step\"\ntrigger = \"cron\"\nschedule = \"0 7 * * 1-5\"\n"
    );
    assert_eq!(
        std::fs::read_to_string(out.join("pack/orders/on-close.toml")).unwrap(),
        "[order]\nformula = \"one-step\"\ntrigger = \"event\"\non = \"bead.closed\"\n"
    );
    // D4: the authored formula the orders reference ships in the pack
    assert_eq!(
        std::fs::read_to_string(out.join("pack/formulas/one-step.toml")).unwrap(),
        "formula = \"one-step\"\n\n[[steps]]\nid = \"s1\"\ntitle = \"one step\"\n"
    );
    assert_eq!(
        (
            report.issues,
            report.memories,
            report.agents,
            report.orders,
            report.pack_formulas
        ),
        (1, 1, 1, 2, 1)
    );
}

#[test]
fn an_order_referencing_a_missing_formula_fails_naming_it() {
    let dir = tempfile::tempdir().unwrap();
    let (camp_root, ledger, config) = fixture_camp(
        dir.path(),
        "\n[[order]]\nname = \"nightly\"\non = \"cron:0 7 * * *\"\nformula = \"ghost\"\n",
    );
    let out = dir.path().join("city");
    match export_city(&ledger, &config, &camp_root, &out, &NO_SKIP) {
        Err(CoreError::Export(msg)) => {
            assert!(msg.contains("ghost") && msg.contains("nightly"), "{msg}")
        }
        other => panic!("expected Export error, got {other:?}"),
    }
}

/// D5: newest pinned copy takes the bare name; an older divergent copy is
/// archived per-run; identical copies dedupe.
#[test]
fn pinned_formula_archive_dedupes_and_suffixes_divergent_copies() {
    let dir = tempfile::tempdir().unwrap();
    let (camp_root, ledger, config) = fixture_camp(dir.path(), "");
    let runs = camp_root.join("runs");
    for (run_id, content) in [
        ("20260701T080000Z-aaaaaa", "formula = \"one-step\"\n# v1\n"),
        ("20260702T080000Z-bbbbbb", "formula = \"one-step\"\n# v2\n"),
        ("20260703T080000Z-cccccc", "formula = \"one-step\"\n# v2\n"),
    ] {
        let run_dir = runs.join(run_id);
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(run_dir.join("one-step.toml"), content).unwrap();
        std::fs::write(run_dir.join("manifest.json"), "{}").unwrap();
    }
    let out = dir.path().join("city");
    let report = export_city(&ledger, &config, &camp_root, &out, &NO_SKIP).unwrap();

    assert_eq!(
        std::fs::read_to_string(out.join("formulas/one-step.toml")).unwrap(),
        "formula = \"one-step\"\n# v2\n",
        "newest run's copy takes the bare name"
    );
    assert_eq!(
        std::fs::read_to_string(out.join("formulas/one-step.20260701T080000Z-aaaaaa.toml"))
            .unwrap(),
        "formula = \"one-step\"\n# v1\n",
        "older divergent copy is archived per-run"
    );
    assert_eq!(report.archive_formulas, 2, "identical copies dedupe");
    assert!(
        report
            .notes
            .iter()
            .any(|n| n.contains("20260701T080000Z-aaaaaa")),
        "divergence is noted: {:?}",
        report.notes
    );
}

#[test]
fn a_run_dir_without_a_pinned_formula_fails() {
    let dir = tempfile::tempdir().unwrap();
    let (camp_root, ledger, config) = fixture_camp(dir.path(), "");
    let run_dir = camp_root.join("runs/20260701T080000Z-aaaaaa");
    std::fs::create_dir_all(&run_dir).unwrap();
    std::fs::write(run_dir.join("manifest.json"), "{}").unwrap();
    let out = dir.path().join("city");
    match export_city(&ledger, &config, &camp_root, &out, &NO_SKIP) {
        Err(CoreError::Export(msg)) => {
            assert!(msg.contains("20260701T080000Z-aaaaaa"), "{msg}")
        }
        other => panic!("expected Export error, got {other:?}"),
    }
}

#[test]
fn a_missing_agents_dir_is_noted_not_fatal() {
    let dir = tempfile::tempdir().unwrap();
    let (camp_root, ledger, config) = fixture_camp(dir.path(), "");
    std::fs::remove_dir_all(camp_root.join("agents")).unwrap();
    let out = dir.path().join("city");
    let report = export_city(&ledger, &config, &camp_root, &out, &NO_SKIP).unwrap();
    assert_eq!(report.agents, 0);
    assert!(
        report
            .notes
            .iter()
            .any(|n| n.contains("no agent definitions")),
        "{:?}",
        report.notes
    );
    assert!(
        out.join("pack/agents").is_dir(),
        "layout stays deterministic"
    );
}

/// PR #18 review finding 3: a symlinked agent definition must fail with
/// an actionable message, not "neither a file nor a directory".
#[cfg(unix)]
#[test]
fn a_symlinked_agent_definition_fails_with_an_actionable_error() {
    let dir = tempfile::tempdir().unwrap();
    let (camp_root, ledger, config) = fixture_camp(dir.path(), "");
    std::os::unix::fs::symlink(
        camp_root.join("agents/dev/prompt.md"),
        camp_root.join("agents/link.md"),
    )
    .unwrap();
    let out = dir.path().join("city");
    match export_city(&ledger, &config, &camp_root, &out, &NO_SKIP) {
        Err(CoreError::Export(msg)) => {
            assert!(msg.contains("symlink") && msg.contains("link.md"), "{msg}")
        }
        other => panic!("expected Export error, got {other:?}"),
    }
}

/// D10: export is read-only — it appends nothing to the ledger.
#[test]
fn export_appends_no_events() {
    let dir = tempfile::tempdir().unwrap();
    let (camp_root, ledger, config) = fixture_camp(dir.path(), TRANSLATABLE_ORDERS);
    let before = ledger.events_range(1, None).unwrap().len();
    let out = dir.path().join("city");
    export_city(&ledger, &config, &camp_root, &out, &NO_SKIP).unwrap();
    assert_eq!(ledger.events_range(1, None).unwrap().len(), before);
}

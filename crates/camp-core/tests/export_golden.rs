#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Phase 14 golden export (master plan: "golden export of a fixture camp —
//! beads incl. closed-with-outcome history, one cooked run, both order
//! kinds; JSONL parses line by line and field-maps exactly").
//!
//! Regenerate after an intentional output change:
//!   UPDATE_EXPORT_GOLDEN=1 cargo test -p camp-core --test export_golden
//! then eyeball `git diff crates/camp-core/tests/fixtures/export-golden/`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use camp_core::clock::FixedClock;
use camp_core::config::{CampConfig, RigConfig};
use camp_core::event::{EventInput, EventType};
use camp_core::export::{ExportOptions, export_city};
use camp_core::formula;
use camp_core::ledger::Ledger;

const TS: &str = "2026-07-05T21:14:03Z";
const GOLDEN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/export-golden");
const FORMULA: &str = "formula = \"one-step\"\n\n[[steps]]\nid = \"s1\"\ntitle = \"one step\"\n";
const AGENT: &str = "# dev\n\nYou are the dev agent for the golden camp.\n";
const ORDERS: &str = r#"
[[order]]
name = "nightly"
on = "cron:0 7 * * 1-5"
formula = "one-step"

[[order]]
name = "on-close"
on = "event:bead.closed"
formula = "one-step"
"#;

fn append(ledger: &mut Ledger, kind: EventType, bead: &str, data: serde_json::Value) {
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

/// Build the fixture camp and export it; returns (out_dir, run_id).
fn export_fixture(dir: &Path) -> (PathBuf, String) {
    let camp_root = dir.join(".camp");
    std::fs::create_dir_all(&camp_root).unwrap();
    let rig_path = dir.join("repo");
    std::fs::create_dir_all(&rig_path).unwrap();
    let config_text = format!(
        "[camp]\nname = \"golden\"\n\n[[rigs]]\nname = \"gc\"\npath = {:?}\nprefix = \"gc\"\n{ORDERS}",
        rig_path.display()
    );
    std::fs::write(camp_root.join("camp.toml"), &config_text).unwrap();
    let config = CampConfig::parse(&config_text).unwrap();

    std::fs::create_dir_all(camp_root.join("formulas")).unwrap();
    std::fs::write(camp_root.join("formulas/one-step.toml"), FORMULA).unwrap();
    std::fs::create_dir_all(camp_root.join("agents")).unwrap();
    std::fs::write(camp_root.join("agents/dev.md"), AGENT).unwrap();

    let mut ledger =
        Ledger::open_with_clock(&camp_root.join("camp.db"), Box::new(FixedClock::new(TS)))
            .unwrap();
    // closed-with-outcome history
    append(
        &mut ledger,
        EventType::BeadCreated,
        "gc-1",
        serde_json::json!({"title": "implement widget", "description": "the change", "labels": ["cli"], "assignee": "dev"}),
    );
    append(
        &mut ledger,
        EventType::BeadClaimed,
        "gc-1",
        serde_json::json!({"session": "camp/dev/1"}),
    );
    append(
        &mut ledger,
        EventType::BeadClosed,
        "gc-1",
        serde_json::json!({"outcome": "pass", "reason": "shipped the widget"}),
    );
    // open + blocked
    append(
        &mut ledger,
        EventType::BeadCreated,
        "gc-2",
        serde_json::json!({"title": "review widget", "needs": ["gc-1"]}),
    );
    // mail + memory
    append(
        &mut ledger,
        EventType::BeadCreated,
        "gc-3",
        serde_json::json!({"title": "ping from ci", "type": "mail"}),
    );
    append(
        &mut ledger,
        EventType::BeadCreated,
        "gc-4",
        serde_json::json!({"title": "deploy needs the VPN profile", "type": "memory"}),
    );
    // one cooked run: pins the formula copy under runs/, creates run beads
    let parsed = formula::parse_and_validate(&camp_root.join("formulas/one-step.toml")).unwrap();
    let rig = RigConfig {
        name: "gc".into(),
        path: rig_path,
        prefix: "gc".into(),
        default_agent: None,
    };
    let cooked = formula::cook(
        &mut ledger,
        &parsed,
        &camp_root.join("runs"),
        &rig,
        "order:nightly:1",
    )
    .unwrap();

    let out = dir.join("city");
    export_city(
        &ledger,
        &config,
        &camp_root,
        &out,
        &ExportOptions {
            skip_untranslatable: false,
        },
    )
    .unwrap();
    (out, cooked.run_id)
}

fn walk(root: &Path) -> BTreeMap<String, String> {
    fn inner(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                inner(root, &path, out);
            } else {
                let rel = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_owned();
                out.insert(rel, std::fs::read_to_string(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    inner(root, root, &mut out);
    out
}

#[test]
fn golden_export_matches_the_checked_in_tree() {
    let dir = tempfile::tempdir().unwrap();
    let (out, run_id) = export_fixture(dir.path());

    // normalize the one nondeterministic value (24 random bits in run ids)
    let actual: BTreeMap<String, String> = walk(&out)
        .into_iter()
        .map(|(path, content)| (path, content.replace(&run_id, "RUNID")))
        .collect();

    if std::env::var_os("UPDATE_EXPORT_GOLDEN").is_some() {
        let golden_root = Path::new(GOLDEN);
        if golden_root.exists() {
            std::fs::remove_dir_all(golden_root).unwrap();
        }
        for (rel, content) in &actual {
            let dest = golden_root.join(rel);
            std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
            std::fs::write(dest, content).unwrap();
        }
        panic!(
            "golden tree regenerated under {GOLDEN} — inspect the diff and rerun without \
             UPDATE_EXPORT_GOLDEN"
        );
    }

    let golden = walk(Path::new(GOLDEN));
    assert_eq!(
        actual.keys().collect::<Vec<_>>(),
        golden.keys().collect::<Vec<_>>(),
        "output file set differs from the golden tree"
    );
    for (rel, content) in &golden {
        assert_eq!(&actual[rel], content, "content mismatch in {rel}");
    }
}

#[test]
fn beads_jsonl_parses_line_by_line_and_field_maps_exactly() {
    let dir = tempfile::tempdir().unwrap();
    let (out, run_id) = export_fixture(dir.path());
    let text = std::fs::read_to_string(out.join("beads.jsonl")).unwrap();
    let lines: Vec<serde_json::Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();

    // every line is a self-contained JSON object with a _type discriminator
    for line in &lines {
        let t = line["_type"].as_str().unwrap();
        assert!(t == "issue" || t == "memory", "unexpected _type {t}");
    }

    // gc-1: the closed-with-outcome issue, field by field (mapping table)
    let gc1 = lines.iter().find(|l| l["id"] == "gc-1").expect("gc-1 line");
    assert_eq!(gc1["title"], "implement widget");
    assert_eq!(gc1["description"], "the change");
    assert_eq!(gc1["status"], "closed");
    assert_eq!(gc1["priority"], 2);
    assert_eq!(gc1["issue_type"], "task");
    assert_eq!(gc1["assignee"], "dev");
    assert_eq!(gc1["created_at"], TS);
    assert_eq!(gc1["updated_at"], TS);
    assert_eq!(gc1["closed_at"], TS);
    assert_eq!(gc1["close_reason"], "shipped the widget");
    assert_eq!(gc1["labels"], serde_json::json!(["cli"]));
    assert_eq!(gc1["metadata"]["gc.outcome"], "pass");
    assert_eq!(gc1["metadata"]["camp.rig"], "gc");
    assert_eq!(gc1["metadata"]["camp.claimed_by"], "camp/dev/1");
    assert!(
        gc1["metadata"].get("gc.final_disposition").is_none(),
        "no merged phase records a final disposition yet (plan D6)"
    );

    // gc-2: the needs edge became a bd blocking dependency
    let gc2 = lines.iter().find(|l| l["id"] == "gc-2").unwrap();
    assert_eq!(
        gc2["dependencies"],
        serde_json::json!([{"issue_id": "gc-2", "depends_on_id": "gc-1", "type": "blocks"}])
    );

    // gc-3: mail → native bd message type
    let gc3 = lines.iter().find(|l| l["id"] == "gc-3").unwrap();
    assert_eq!(gc3["issue_type"], "message");

    // gc-4: memory → native bd memory record, not an issue
    let mem = lines
        .iter()
        .find(|l| l["_type"] == "memory")
        .expect("memory record");
    assert_eq!(mem["key"], "gc-4");
    assert_eq!(mem["value"], "deploy needs the VPN profile");
    assert!(mem.get("id").is_none());

    // cooked-run beads carry run/step provenance in camp.* metadata
    let step = lines
        .iter()
        .find(|l| l["metadata"]["camp.step_id"] == "s1")
        .expect("step bead line");
    assert_eq!(step["metadata"]["camp.run_id"], run_id);
}

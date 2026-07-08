//! Cook-side ledger behavior: the run.cooked event, run-aware bead.created,
//! and the cook() transaction itself.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use camp_core::clock::FixedClock;
use camp_core::event::{EventInput, EventType};
use camp_core::ledger::Ledger;

fn temp_ledger() -> (tempfile::TempDir, Ledger) {
    let dir = tempfile::tempdir().unwrap();
    let ledger = Ledger::open_with_clock(
        &dir.path().join("camp.db"),
        Box::new(FixedClock::new("2026-07-05T21:14:03Z")),
    )
    .unwrap();
    (dir, ledger)
}

#[test]
fn run_cooked_round_trips_and_is_log_only() {
    let (_dir, mut ledger) = temp_ledger();
    assert_eq!(
        EventType::parse("run.cooked").unwrap(),
        EventType::RunCooked
    );
    ledger
        .append(EventInput {
            kind: EventType::RunCooked,
            rig: Some("gc".into()),
            actor: "cli".into(),
            bead: None,
            data: serde_json::json!({
                "run_id": "20260705T211403Z-a1b2c3",
                "formula": "minimal",
                "root": "gc-1",
                "steps": {"only": "gc-2"}
            }),
        })
        .unwrap();
    // log-only: no bead rows appear
    let beads = ledger.list_beads(&Default::default()).unwrap();
    assert!(beads.is_empty());
    let events = ledger.events_range(1, None).unwrap();
    assert_eq!(events.len(), 1);
}

#[test]
fn run_cooked_payload_is_validated_and_rejects_unknown_fields() {
    let (_dir, mut ledger) = temp_ledger();
    for bad in [
        serde_json::json!({"formula": "m", "root": "gc-1", "steps": {}}), // missing run_id
        serde_json::json!({"run_id": "", "formula": "m", "root": "gc-1", "steps": {}}), // empty
        serde_json::json!({"run_id": "r", "formula": "m", "root": "gc-1", "steps": {}, "extra": 1}),
    ] {
        assert!(
            ledger
                .append(EventInput {
                    kind: EventType::RunCooked,
                    rig: Some("gc".into()),
                    actor: "cli".into(),
                    bead: None,
                    data: bad.clone(),
                })
                .is_err(),
            "must reject {bad}"
        );
    }
    assert!(ledger.events_range(1, None).unwrap().is_empty());
}

#[test]
fn bead_created_accepts_run_and_step_ids_and_refolds_exactly() {
    let (_dir, mut ledger) = temp_ledger();
    ledger
        .append(EventInput {
            kind: EventType::BeadCreated,
            rig: Some("gc".into()),
            actor: "cli".into(),
            bead: Some("gc-1".into()),
            data: serde_json::json!({
                "title": "implement",
                "run_id": "20260705T211403Z-a1b2c3",
                "step_id": "implement"
            }),
        })
        .unwrap();
    let report = ledger.refold_check().unwrap();
    assert!(report.drift.is_empty(), "{:?}", report.drift);
}

use camp_core::config::RigConfig;
use camp_core::formula::{cook, parse_and_validate};

fn fixture(stem: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/formulas/valid")
        .join(format!("{stem}.toml"))
}

fn rig() -> RigConfig {
    RigConfig {
        name: "gascity".into(),
        path: "/code/gascity".into(),
        prefix: "gc".into(),
        default_agent: None,
    }
}

#[test]
fn cook_materializes_a_diamond_run_in_one_transaction() {
    let (dir, mut ledger) = temp_ledger();
    let formula = parse_and_validate(&fixture("diamond")).unwrap();
    let runs = dir.path().join("runs");
    let cooked = cook(&mut ledger, &formula, &runs, &rig(), "cli").unwrap();

    // run_id shape: utc-compact from the FixedClock + 6 hex
    assert!(
        cooked.run_id.starts_with("20260705T211403Z-"),
        "{}",
        cooked.run_id
    );
    let suffix = cooked.run_id.rsplit('-').next().unwrap();
    assert_eq!(suffix.len(), 6);
    assert!(
        suffix
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    );

    // beads: root + 4 steps, contiguous rig-prefixed ids
    assert_eq!(cooked.root_bead, "gc-1");
    assert_eq!(cooked.step_beads.len(), 4);
    assert_eq!(cooked.step_beads["design"], "gc-2");
    assert_eq!(cooked.step_beads["release"], "gc-5");

    // step bead carries assignee and the rig name; needs are bead ids
    let release = ledger.get_bead("gc-5").unwrap().unwrap();
    assert_eq!(release.assignee.as_deref(), Some("dev"));
    assert_eq!(release.rig, "gascity");

    // events: 5 creates + run.cooked, all in one batch
    let events = ledger.events_range(1, None).unwrap();
    assert_eq!(events.len(), 6);
    assert_eq!(events[0].bead.as_deref(), Some("gc-1"));
    assert_eq!(events[5].kind, EventType::RunCooked);
    assert_eq!(events[5].bead.as_deref(), Some("gc-1"));
    assert_eq!(events[5].data["steps"]["design"], "gc-2");

    // refold property holds over a cooked ledger
    assert!(ledger.refold_check().unwrap().drift.is_empty());
}

#[test]
fn cooked_graphs_satisfy_phase_3_readiness_roots_ready_dependents_not() {
    let (dir, mut ledger) = temp_ledger();
    let formula = parse_and_validate(&fixture("diamond")).unwrap();
    let cooked = cook(
        &mut ledger,
        &formula,
        &dir.path().join("runs"),
        &rig(),
        "cli",
    )
    .unwrap();

    let ready: Vec<String> = ledger
        .ready_beads(None)
        .unwrap()
        .into_iter()
        .map(|b| b.id)
        .collect();
    // Only the dag root step (design) is ready. Dependents are blocked, and
    // the run root needs every step, so it is blocked too.
    assert_eq!(ready, vec![cooked.step_beads["design"].clone()]);
    assert!(!ledger.is_ready(&cooked.root_bead).unwrap());
    assert!(!ledger.is_ready(&cooked.step_beads["implement"]).unwrap());
    assert!(!ledger.is_ready(&cooked.step_beads["release"]).unwrap());
}

#[test]
fn cook_pins_the_formula_verbatim_and_writes_the_manifest() {
    let (dir, mut ledger) = temp_ledger();
    let source_path = fixture("guarded-change");
    let formula = parse_and_validate(&source_path).unwrap();
    let runs = dir.path().join("runs");
    let cooked = cook(&mut ledger, &formula, &runs, &rig(), "cli").unwrap();

    let run_dir = runs.join(&cooked.run_id);
    let pinned = std::fs::read_to_string(run_dir.join("guarded-change.toml")).unwrap();
    assert_eq!(
        pinned,
        std::fs::read_to_string(&source_path).unwrap(),
        "verbatim copy"
    );

    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("manifest.json")).unwrap())
            .unwrap();
    assert_eq!(manifest["run_id"], cooked.run_id.as_str());
    assert_eq!(manifest["formula"], "guarded-change");
    assert_eq!(manifest["rig"], "gascity");
    assert_eq!(manifest["actor"], "cli");
    assert_eq!(manifest["cooked_ts"], "2026-07-05T21:14:03Z");
    assert_eq!(manifest["root"], cooked.root_bead.as_str());
    assert_eq!(
        manifest["steps"]["implement"],
        cooked.step_beads["implement"].as_str()
    );
}

#[test]
fn cook_is_file_independent_afterwards() {
    let (dir, mut ledger) = temp_ledger();
    // copy the fixture somewhere deletable, cook it, delete the original
    let scratch = dir.path().join("minimal.toml");
    std::fs::copy(fixture("minimal"), &scratch).unwrap();
    let formula = parse_and_validate(&scratch).unwrap();
    let cooked = cook(
        &mut ledger,
        &formula,
        &dir.path().join("runs"),
        &rig(),
        "cli",
    )
    .unwrap();
    std::fs::remove_file(&scratch).unwrap();

    // the run lives on: beads dispatchable, pinned copy present
    assert!(ledger.is_ready(&cooked.step_beads["only"]).unwrap());
    assert!(
        dir.path()
            .join("runs")
            .join(&cooked.run_id)
            .join("minimal.toml")
            .exists()
    );
}

#[test]
fn cook_rejects_unknown_needs_ids_in_hand_built_formulas() {
    // Review finding 1: a caller constructing Formula directly (bypassing
    // parse_and_validate) must not get a bead silently missing an edge.
    use camp_core::formula::{Formula, Step};
    let (dir, mut ledger) = temp_ledger();
    let step = |id: &str, needs: &[&str]| Step {
        id: id.into(),
        title: "t".into(),
        description: None,
        needs: needs.iter().map(|s| (*s).to_owned()).collect(),
        assignee: None,
        timeout: None,
        check: None,
        retry: None,
        on_complete: None,
    };
    let formula = Formula {
        name: "hand".into(),
        description: None,
        requires: None,
        steps: vec![step("a", &[]), step("b", &["ghost"])],
        source: String::new(),
    };
    let runs = dir.path().join("runs");
    let err = cook(&mut ledger, &formula, &runs, &rig(), "cli").unwrap_err();
    assert!(
        matches!(err, camp_core::error::CoreError::Cook(_)),
        "want Cook error, got {err:?}"
    );
    assert!(err.to_string().contains("ghost"), "{err}");
    // fail fast: nothing landed — no events, no beads, no run dir
    assert!(ledger.events_range(1, None).unwrap().is_empty());
    assert!(!runs.exists() || std::fs::read_dir(&runs).unwrap().count() == 0);
}

#[test]
fn cook_survives_a_run_id_collision_by_regenerating_the_suffix() {
    // Review finding 6: same-second suffix collision retries once instead
    // of failing with "File exists". fastrand's thread-local RNG makes the
    // collision deterministic: learn the first draw, re-seed, pre-create.
    let (dir, mut ledger) = temp_ledger();
    fastrand::seed(7);
    let colliding = format!("20260705T211403Z-{:06x}", fastrand::u32(..) & 0xFF_FFFF);
    let runs = dir.path().join("runs");
    std::fs::create_dir_all(runs.join(&colliding)).unwrap();

    fastrand::seed(7); // cook's first draw now collides
    let formula = parse_and_validate(&fixture("minimal")).unwrap();
    let cooked = cook(&mut ledger, &formula, &runs, &rig(), "cli").unwrap();
    assert_ne!(cooked.run_id, colliding, "suffix must be regenerated");
    assert!(runs.join(&cooked.run_id).join("minimal.toml").exists());
}

#[test]
fn cook_fs_failures_are_not_reported_as_ledger_corruption() {
    // Review finding 4: disk trouble during cook must not read as a damaged
    // ledger. run_dir here is a FILE, so create_dir_all fails.
    let (dir, mut ledger) = temp_ledger();
    let blocker = dir.path().join("runs");
    std::fs::write(&blocker, b"not a directory").unwrap();
    let formula = parse_and_validate(&fixture("minimal")).unwrap();
    let err = cook(&mut ledger, &formula, &blocker, &rig(), "cli").unwrap_err();
    assert!(
        matches!(err, camp_core::error::CoreError::Cook(_)),
        "want Cook error, got {err:?}"
    );
    let text = err.to_string();
    assert!(text.starts_with("cook:"), "{text}");
    assert!(!text.contains("ledger corrupt"), "{text}");
}

#[test]
fn cook_atomicity_fault_injection_leaves_nothing() {
    let (dir, mut ledger) = temp_ledger();
    // Occupy gc-2 through the public API (the counter advances to 2), then
    // FAULT-INJECT by winding the counter back with a raw connection so
    // cook allocates a block that collides with gc-2 mid-batch: root gc-1
    // inserts fine, the first step create hits the gc-2 primary key, and
    // the whole transaction must roll back.
    ledger
        .append(EventInput {
            kind: EventType::BeadCreated,
            rig: Some("gascity".into()),
            actor: "test".into(),
            bead: Some("gc-2".into()),
            data: serde_json::json!({"title": "squatter"}),
        })
        .unwrap();
    let db = dir.path().join("camp.db");
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute("UPDATE counters SET high = 0 WHERE prefix = 'gc'", [])
            .unwrap();
    }
    let events_before = ledger.events_range(1, None).unwrap().len();

    let formula = parse_and_validate(&fixture("diamond")).unwrap();
    let runs = dir.path().join("runs");
    let err = cook(&mut ledger, &formula, &runs, &rig(), "cli");
    assert!(err.is_err(), "colliding id block must fail the whole cook");

    // NOTHING landed: no new events, no beads beyond the squatter, no run dir
    assert_eq!(ledger.events_range(1, None).unwrap().len(), events_before);
    assert_eq!(ledger.list_beads(&Default::default()).unwrap().len(), 1);
    let leftover = std::fs::read_dir(&runs).map(|d| d.count()).unwrap_or(0);
    assert_eq!(leftover, 0, "run dir must be removed on rollback");

    // The injected counter tamper is exactly what doctor --refold repairs;
    // after repair the ledger is drift-free and cooking works again.
    ledger.refold_repair().unwrap();
    assert!(ledger.refold_check().unwrap().drift.is_empty());
    let cooked = cook(&mut ledger, &formula, &runs, &rig(), "cli").unwrap();
    assert_eq!(cooked.root_bead, "gc-3");
}

// ---- Phase 9 Task 3: cook options (vars, root linkage) --------------------

#[test]
fn cook_with_substitutes_vars_and_links_the_root() {
    use camp_core::formula::{CookOptions, cook_with};
    let (dir, mut ledger) = temp_ledger();
    // a pre-existing bead the child root will need (the previous bond child)
    ledger
        .append(EventInput {
            kind: EventType::BeadCreated,
            rig: Some("gc".into()),
            actor: "test".into(),
            bead: Some("gc-1".into()),
            data: serde_json::json!({"title": "previous child root"}),
        })
        .unwrap();

    let source = "formula = \"child\"\n\n[[steps]]\nid = \"work\"\n\
                  title = \"Handle {name} at {position}\"\ndescription = \"for {name}\"\n";
    let path = dir.path().join("child.toml");
    std::fs::write(&path, source).unwrap();
    let formula = parse_and_validate(&path).unwrap();

    let mut vars = std::collections::BTreeMap::new();
    vars.insert("name".to_owned(), "alpha".to_owned());
    vars.insert("position".to_owned(), "0".to_owned());
    let opts = CookOptions {
        vars,
        extra_root_needs: vec!["gc-1".to_owned()],
        extra_root_labels: vec!["bond:gc-1:0".to_owned()],
    };
    let runs = dir.path().join("runs");
    let cooked = cook_with(&mut ledger, &formula, &runs, &rig(), "campd", &opts).unwrap();

    // vars substituted into the step bead's title and description
    let step = ledger
        .get_bead(&cooked.step_beads["work"])
        .unwrap()
        .unwrap();
    assert_eq!(step.title, "Handle alpha at 0");
    let events = ledger.events_range(1, None).unwrap();
    let step_created = events
        .iter()
        .find(|e| e.kind == EventType::BeadCreated && e.bead.as_deref() == Some(step.id.as_str()))
        .unwrap();
    assert_eq!(step_created.data["description"], "for alpha");

    // root carries the bond label and the extra needs edge
    let root = ledger.get_bead(&cooked.root_bead).unwrap().unwrap();
    assert_eq!(root.labels, vec!["bond:gc-1:0".to_owned()]);
    let root_created = events
        .iter()
        .find(|e| e.kind == EventType::BeadCreated && e.bead.as_deref() == Some(root.id.as_str()))
        .unwrap();
    let needs: Vec<String> = root_created.data["needs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    assert!(needs.contains(&"gc-1".to_owned()), "needs: {needs:?}");
    assert!(
        needs.contains(&cooked.step_beads["work"]),
        "needs: {needs:?}"
    );

    // the pinned file stays byte-verbatim (materialization property)
    let pinned = std::fs::read_to_string(runs.join(&cooked.run_id).join("child.toml")).unwrap();
    assert_eq!(pinned, source);

    // the manifest records the substituted vars
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(runs.join(&cooked.run_id).join("manifest.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest["vars"]["name"], "alpha");

    // the loaded RunContext round-trips (runtime consumes bond manifests too)
    let ctx = camp_core::formula::runtime::load_run(&runs, &cooked.run_id).unwrap();
    assert_eq!(ctx.anchors, cooked.step_beads);

    // refold property holds over the whole story
    assert!(ledger.refold_check().unwrap().drift.is_empty());
}

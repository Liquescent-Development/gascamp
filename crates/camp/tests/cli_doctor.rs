#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![allow(non_snake_case)]

use assert_cmd::Command;
use camp_core::clock::FixedClock;
use camp_core::event::{EventInput, EventType};
use camp_core::ledger::Ledger;

fn camp() -> Command {
    let mut cmd = Command::cargo_bin("camp").unwrap();
    cmd.env_remove("CAMP_DIR");
    cmd
}

fn seeded_camp(dir: &std::path::Path) -> std::path::PathBuf {
    camp()
        .current_dir(dir)
        .args(["init", "--no-service"])
        .assert()
        .success();
    let camp_root = dir.join(".camp");
    let mut ledger = Ledger::open_with_clock(
        &camp_root.join("camp.db"),
        Box::new(FixedClock::new("2026-07-05T21:14:03Z")),
    )
    .unwrap();
    ledger
        .append(EventInput {
            kind: EventType::BeadCreated,
            rig: Some("gc".into()),
            actor: "test".into(),
            bead: Some("gc-1".into()),
            data: serde_json::json!({"title": "one"}),
        })
        .unwrap();
    ledger
        .append(EventInput {
            kind: EventType::BeadClosed,
            rig: Some("gc".into()),
            actor: "test".into(),
            bead: Some("gc-1".into()),
            data: serde_json::json!({"outcome": "pass"}),
        })
        .unwrap();
    camp_root
}

/// Manufacture `runs/<id>/` the way cook writes it — files FIRST. `cooked`
/// additionally appends the `run.cooked` event naming the id, which is the ONE
/// difference between a live run and a crash orphan (#124).
fn make_run_dir(camp_root: &std::path::Path, run_id: &str, cooked: bool) -> std::path::PathBuf {
    let dir = camp_root.join("runs").join(run_id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("manifest.json"),
        format!("{{\"run_id\":\"{run_id}\"}}"),
    )
    .unwrap();
    if cooked {
        let mut ledger = Ledger::open_with_clock(
            &camp_root.join("camp.db"),
            Box::new(FixedClock::new("2026-07-05T21:14:03Z")),
        )
        .unwrap();
        ledger
            .append(EventInput {
                kind: EventType::RunCooked,
                rig: Some("gc".into()),
                actor: "test".into(),
                bead: Some("gc-1".into()),
                data: serde_json::json!({
                    "run_id": run_id, "formula": "build", "root": "gc-1", "steps": {},
                }),
            })
            .unwrap();
    }
    dir
}

/// Push a run dir's mtime into the past. The sweep's grace window asks exactly
/// one question — "could a cook still be writing in here?" — and mtime is the
/// only evidence there is.
fn age(dir: &std::path::Path, secs: u64) {
    let f = std::fs::File::open(dir).unwrap();
    let past = std::time::SystemTime::now() - std::time::Duration::from_secs(secs);
    f.set_times(std::fs::FileTimes::new().set_modified(past))
        .unwrap();
}

fn doctor_stdout(dir: &std::path::Path, args: &[&str]) -> String {
    let out = camp().current_dir(dir).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "camp {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn tamper(camp_root: &std::path::Path) {
    let conn = rusqlite::Connection::open(camp_root.join("camp.db")).unwrap();
    conn.execute(
        "UPDATE beads SET status = 'open', outcome = NULL WHERE id = 'gc-1'",
        [],
    )
    .unwrap();
}

#[test]
fn doctor_refold_reports_clean_on_a_healthy_ledger() {
    let dir = tempfile::tempdir().unwrap();
    seeded_camp(dir.path());
    camp()
        .current_dir(dir.path())
        .args(["doctor", "--refold"])
        .assert()
        .success()
        .stdout(predicates::str::contains("replayed 2 events; 0 drift rows"));
}

#[test]
fn doctor_refold_detects_drift_and_exits_1() {
    let dir = tempfile::tempdir().unwrap();
    let camp_root = seeded_camp(dir.path());
    tamper(&camp_root);
    camp()
        .current_dir(dir.path())
        .args(["doctor", "--refold"])
        .assert()
        .failure()
        .code(1)
        .stdout(predicates::str::contains("gc-1"))
        .stderr(predicates::str::contains("drift"));
}

#[test]
fn doctor_refold_repair_rebuilds_and_subsequent_check_is_clean() {
    let dir = tempfile::tempdir().unwrap();
    let camp_root = seeded_camp(dir.path());
    tamper(&camp_root);
    camp()
        .current_dir(dir.path())
        .args(["doctor", "--refold", "--repair"])
        .assert()
        .success()
        .stdout(predicates::str::contains("0 drift rows"));
    camp()
        .current_dir(dir.path())
        .args(["doctor", "--refold"])
        .assert()
        .success();
}

#[test]
fn doctor_refold_repair_restores_BEAD_METADATA_including_a_drain_reservation() {
    // The `bead_meta` refold entry was defended by exactly ONE proptest: deleting its
    // `TableSpec` failed only `refold_prop`, and no CLI-level `doctor --refold` test
    // carried metadata at all. `--refold` is the operator's integrity surface; if it
    // cannot see a reservation, `--repair` silently drops every one of them (and, with
    // `foreign_keys = ON`, hard-fails on the FK if the spec is ordered wrong).
    //
    // This drives the REAL binary end to end: metadata (incl. gc's reservation key)
    // is written, corrupted in the state table, DETECTED as drift, and RESTORED.
    let dir = tempfile::tempdir().unwrap();
    let camp_root = seeded_camp(dir.path());

    {
        let mut ledger = Ledger::open_with_clock(
            &camp_root.join("camp.db"),
            Box::new(FixedClock::new("2026-07-05T21:14:03Z")),
        )
        .unwrap();
        ledger
            .append(EventInput {
                kind: EventType::BeadCreated,
                rig: Some("gc".into()),
                actor: "test".into(),
                bead: Some("gc-9".into()),
                data: serde_json::json!({"title": "member"}),
            })
            .unwrap();
        ledger
            .append(EventInput {
                kind: EventType::BeadUpdated,
                rig: Some("gc".into()),
                actor: "campd".into(),
                bead: Some("gc-9".into()),
                data: serde_json::json!({
                    "metadata": {
                        camp_core::readiness::EXCLUSIVE_DRAIN_RESERVATION: "gc-3",
                        "gc.run_target": "superpowers.implementer",
                    }
                }),
            })
            .unwrap();
    }

    // A healthy ledger refolds CLEAN — the metadata round-trips through the shadow.
    camp()
        .current_dir(dir.path())
        .args(["doctor", "--refold"])
        .assert()
        .success();

    // CORRUPT the state table behind the fold's back.
    {
        let conn = rusqlite::Connection::open(camp_root.join("camp.db")).unwrap();
        conn.execute(
            "UPDATE bead_meta SET value = 'gc-WRONG' WHERE bead_id = 'gc-9' AND key = ?1",
            [camp_core::readiness::EXCLUSIVE_DRAIN_RESERVATION],
        )
        .unwrap();
    }

    // …the refold must SEE it (a state table it does not diff is a state table it
    // cannot repair).
    camp()
        .current_dir(dir.path())
        .args(["doctor", "--refold"])
        .assert()
        .failure()
        .stdout(predicates::str::contains("bead_meta"));

    // …and --repair must RESTORE it.
    camp()
        .current_dir(dir.path())
        .args(["doctor", "--refold", "--repair"])
        .assert()
        .success();

    let ledger = Ledger::open_read_only(&camp_root.join("camp.db")).unwrap();
    let md = ledger.bead_metadata("gc-9").unwrap();
    assert_eq!(
        md.get(camp_core::readiness::EXCLUSIVE_DRAIN_RESERVATION)
            .map(String::as_str),
        Some("gc-3"),
        "the reservation is restored from the event log"
    );
    assert_eq!(
        md.get("gc.run_target").map(String::as_str),
        Some("superpowers.implementer")
    );
}

/// The same defect class as the test above, for `beads.final_disposition`
/// (#122): `refold::STATE_TABLES` names the `beads` columns EXPLICITLY, and a
/// column missing from that list is a column `--refold` cannot diff and
/// `--repair` silently NULLs out — `replace_state_from_shadow` inserts only the
/// listed columns, so a rebuild would drop every disposition camp ever
/// recorded. "0 drift rows" over an undiffed column is a vacuous pass, which is
/// exactly how this hides.
///
/// Drives the REAL binary: a disposition is recorded, corrupted behind the
/// fold's back, DETECTED as drift, and RESTORED from the event log.
///
/// Mutation caught: remove `final_disposition` from the `beads` `cols` in
/// refold.rs — the drift goes unseen (`--refold` exits 0) → RED.
#[test]
fn doctor_refold_repair_restores_a_beads_FINAL_DISPOSITION() {
    let dir = tempfile::tempdir().unwrap();
    let camp_root = seeded_camp(dir.path());

    {
        let mut ledger = Ledger::open_with_clock(
            &camp_root.join("camp.db"),
            Box::new(FixedClock::new("2026-07-05T21:14:03Z")),
        )
        .unwrap();
        ledger
            .append(EventInput {
                kind: EventType::BeadCreated,
                rig: Some("gc".into()),
                actor: "test".into(),
                bead: Some("gc-9".into()),
                data: serde_json::json!({"title": "exhausted"}),
            })
            .unwrap();
        ledger
            .append(EventInput {
                kind: EventType::BeadClosed,
                rig: Some("gc".into()),
                actor: "campd".into(),
                bead: Some("gc-9".into()),
                data: serde_json::json!({
                    "outcome": "fail",
                    "final_disposition": "soft_fail",
                    "reason": "check budget (2) exhausted",
                }),
            })
            .unwrap();
    }

    // A healthy ledger refolds CLEAN — the disposition round-trips through the shadow.
    camp()
        .current_dir(dir.path())
        .args(["doctor", "--refold"])
        .assert()
        .success();

    // CORRUPT the column behind the fold's back.
    {
        let conn = rusqlite::Connection::open(camp_root.join("camp.db")).unwrap();
        conn.execute(
            "UPDATE beads SET final_disposition = 'hard_fail' WHERE id = 'gc-9'",
            [],
        )
        .unwrap();
    }

    // …the refold must SEE it (a column it does not diff is a column it cannot repair).
    camp()
        .current_dir(dir.path())
        .args(["doctor", "--refold"])
        .assert()
        .failure()
        .stdout(predicates::str::contains("gc-9"));

    // …and --repair must RESTORE it from the log, not drop it.
    camp()
        .current_dir(dir.path())
        .args(["doctor", "--refold", "--repair"])
        .assert()
        .success();

    let ledger = Ledger::open_read_only(&camp_root.join("camp.db")).unwrap();
    assert_eq!(
        ledger
            .bead_metadata("gc-9")
            .unwrap()
            .get("gc.final_disposition")
            .map(String::as_str),
        Some("soft_fail"),
        "the disposition is restored from the event log, not NULLed by the rebuild"
    );
}

// ---------------------------------------------------------------- #124: orphan run dirs
//
// cook writes `runs/<id>/` BEFORE its one `append_batch` (the deliberate safe
// ordering — a DB-first cook could commit a run whose pinned formula never hit
// disk). A `kill -9` in that window leaves a run dir no ledger row names.
// Recovery is already idempotent; the DIRECTORY is what leaks.

/// Mutation caught: make `orphaned_run_dirs` return an empty Vec unconditionally
/// (or drop the `!cooked.contains(&run_id)` filter's negation) → the listing goes
/// silent → RED.
#[test]
fn doctor_orphan_runs_LISTS_a_run_dir_that_no_RUN_COOKED_EVENT_NAMES() {
    let dir = tempfile::tempdir().unwrap();
    let camp_root = seeded_camp(dir.path());
    let orphan = make_run_dir(&camp_root, "20260705T211403-abc123", false);
    age(&orphan, 3600);

    let out = doctor_stdout(dir.path(), &["doctor", "--orphan-runs"]);
    assert!(out.contains("ORPHAN"), "{out}");
    assert!(out.contains("20260705T211403-abc123"), "{out}");
}

/// THE false-positive guard, and the test that matters most here: a run dir the
/// ledger KNOWS ABOUT must never be called an orphan. Everything downstream of
/// this listing deletes directories; a detector that cannot tell a live run from
/// a crash leftover is a detector that eats live run state.
///
/// Mutation caught: invert the filter to `cooked.contains(&run_id)`, or derive
/// the id set from anything but the ledger — the live run is listed → RED.
#[test]
fn doctor_orphan_runs_does_NOT_list_a_dir_whose_RUN_COOKED_EVENT_EXISTS() {
    let dir = tempfile::tempdir().unwrap();
    let camp_root = seeded_camp(dir.path());
    let live = make_run_dir(&camp_root, "20260705T211403-live01", true);
    // Old enough to be sweepable if it were ever mistaken for an orphan: age is
    // NOT what makes a run live — the ledger is.
    age(&live, 3600);

    let out = doctor_stdout(dir.path(), &["doctor", "--orphan-runs"]);
    assert!(out.contains("no orphaned run directories"), "{out}");
    assert!(
        !out.contains("20260705T211403-live01"),
        "a run named by a run.cooked event is NOT an orphan: {out}"
    );
    assert!(live.exists());
}

/// Mutation caught: have the listing path call the sweep → the dir vanishes → RED.
#[test]
fn doctor_orphan_runs_LISTING_ALONE_DELETES_NOTHING() {
    let dir = tempfile::tempdir().unwrap();
    let camp_root = seeded_camp(dir.path());
    let orphan = make_run_dir(&camp_root, "20260705T211403-orph01", false);
    // Past the grace window: this dir IS sweepable, and listing still must not
    // touch it. Read-only is the DEFAULT, not a consequence of nothing matching.
    age(&orphan, 3600);

    let out = doctor_stdout(dir.path(), &["doctor", "--orphan-runs"]);
    assert!(out.contains("ORPHAN"), "{out}");
    assert!(orphan.exists(), "listing must never delete");
    assert!(
        orphan.join("manifest.json").exists(),
        "…not the dir, not its contents"
    );
}

/// Mutation caught: drop the `!cooked.contains` filter in the SWEEP path (or
/// sweep every dir under runs/) → the live run is removed → RED.
#[test]
fn doctor_sweep_orphan_runs_removes_the_ORPHAN_and_leaves_the_LIVE_run_INTACT() {
    let dir = tempfile::tempdir().unwrap();
    let camp_root = seeded_camp(dir.path());
    let orphan = make_run_dir(&camp_root, "20260705T211403-orph01", false);
    let live = make_run_dir(&camp_root, "20260705T211403-live01", true);
    age(&orphan, 3600);
    age(&live, 3600);

    let out = doctor_stdout(
        dir.path(),
        &["doctor", "--orphan-runs", "--sweep-orphan-runs"],
    );
    assert!(out.contains("swept 1"), "{out}");
    assert!(!orphan.exists(), "the orphan is gone");
    assert!(
        live.exists(),
        "a run dir the ledger names is NEVER swept: {out}"
    );

    // Idempotent: a second sweep finds nothing and still leaves the live run.
    let again = doctor_stdout(
        dir.path(),
        &["doctor", "--orphan-runs", "--sweep-orphan-runs"],
    );
    assert!(again.contains("swept 0"), "{again}");
    assert!(live.exists());
}

/// THE RACE. A run dir with no `run.cooked` is EXACTLY the state a healthy
/// in-flight cook is in for the moment between its run-dir write and its
/// commit. `camp sling` cooks with campd down, so "campd is stopped" alone does
/// not close that window — a freshly-touched dir is never sweepable.
///
/// Mutation caught: delete the `sweepable()` / grace-window check → the dir a
/// cook may still be writing is deleted → RED.
#[test]
fn doctor_sweep_orphan_runs_REFUSES_a_dir_INSIDE_THE_GRACE_WINDOW() {
    let dir = tempfile::tempdir().unwrap();
    let camp_root = seeded_camp(dir.path());
    // No age(): "modified just now" is indistinguishable from "a cook is
    // writing here right now", which is the whole point.
    let fresh = make_run_dir(&camp_root, "20260705T211403-fresh1", false);

    let out = doctor_stdout(
        dir.path(),
        &["doctor", "--orphan-runs", "--sweep-orphan-runs"],
    );
    assert!(
        fresh.exists(),
        "a dir a cook could still be writing is NEVER swept: {out}"
    );
    assert!(out.contains("TOO YOUNG"), "{out}");
    assert!(out.contains("swept 0"), "{out}");
}

#[test]
fn doctor_orphan_runs_on_a_camp_that_has_never_cooked_reports_clean() {
    let dir = tempfile::tempdir().unwrap();
    seeded_camp(dir.path());
    // No runs/ directory at all — the scan must not invent one or error.
    let out = doctor_stdout(dir.path(), &["doctor", "--orphan-runs"]);
    assert!(out.contains("no orphaned run directories"), "{out}");
    assert!(!dir.path().join(".camp/runs").exists());
}

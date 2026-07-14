//! The DRAIN RUNTIME (compat §9 rung 2e): campd-held anchors, all-or-nothing
//! reservations, gather, and the orphan sweep.
//!
//! Every test here drives the REAL campd against a REAL imported pack. The
//! reason is BD8's lesson: rev 2's drain fixtures were layer-free camp-local
//! packs that happened to re-parse cleanly, so the entire class of bug that
//! killed every corpus run was invisible to them. This pack is IMPORTED.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![allow(non_snake_case)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use camp_core::readiness::{BeadRow, EXCLUSIVE_DRAIN_RESERVATION};

const BIN: &str = env!("CARGO_BIN_EXE_camp");

fn fake_agent() -> String {
    format!("{}/tests/fake-agent.sh", env!("CARGO_MANIFEST_DIR"))
}

fn drainfix() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/drainfix")
}

/// The composite harness `daemon_dispatch.rs` does not have: that file has free
/// functions and a `Daemon` with one method and no accessors.
struct Camp {
    _dir: tempfile::TempDir,
    root: PathBuf,
    child: Option<Child>,
}

impl Camp {
    fn new() -> Camp {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".camp");
        std::fs::create_dir_all(&root).unwrap();
        let rig = dir.path().join("repo");
        std::fs::create_dir_all(&rig).unwrap();
        std::fs::write(
            root.join("camp.toml"),
            format!(
                "[camp]\nname = \"t\"\n\n[[rigs]]\nname = \"gc\"\npath = \"{}\"\nprefix = \"gc\"\n\n\
                 [agent_defaults]\ntools = [\"Read\", \"Bash\"]\n\n\
                 [dispatch]\nmax_workers = 4\ncommand = \"{}\"\ndefault_agent = \"dev\"\n",
                rig.display(),
                fake_agent(),
            ),
        )
        .unwrap();
        let agent = root.join("agents/dev");
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(agent.join("agent.toml"), "isolation: none\n").unwrap();
        std::fs::write(agent.join("prompt.md"), "do the work\n").unwrap();

        let c = Camp {
            _dir: dir,
            root,
            child: None,
        };
        c.camp_ok(&["events", "--json"]); // create the ledger
        c.camp_ok(&[
            "import",
            "add",
            drainfix().to_str().unwrap(),
            "--name",
            "fix",
        ]);
        c
    }

    fn camp(&self, args: &[&str]) -> std::process::Output {
        Command::new(BIN)
            .env_remove("CAMP_DIR")
            .arg("--camp")
            .arg(&self.root)
            .args(args)
            .output()
            .unwrap()
    }

    fn camp_ok(&self, args: &[&str]) -> String {
        let out = self.camp(args);
        assert!(
            out.status.success(),
            "camp {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn spawn_campd(&mut self) {
        let mut cmd = Command::new(BIN);
        cmd.env_remove("CAMP_DIR")
            .env("CAMP_BIN", BIN)
            .arg("--camp")
            .arg(&self.root)
            .arg("daemon")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = cmd.spawn().unwrap();
        // block on readiness: campd prints its socket line
        use std::io::{BufRead, BufReader};
        let stdout = child.stdout.take().unwrap();
        let mut lines = BufReader::new(stdout).lines();
        lines.next();
        self.child = Some(child);
    }

    fn stop_campd(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = self.camp(&["stop"]);
            let _ = child.wait();
        }
    }

    fn restart_campd(&mut self) {
        self.stop_campd();
        self.spawn_campd();
    }

    fn conn(&self) -> rusqlite::Connection {
        rusqlite::Connection::open_with_flags(
            self.root.join("camp.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
    }

    fn sling(&self, formula: &str) -> String {
        let out = self.camp_ok(&["sling", "--formula", formula]);
        out.split_whitespace().next().unwrap().to_owned()
    }

    /// `camp create <title> --run <run>` — a run MEMBER (D3).
    fn create_member(&self, run: &str, title: &str) -> String {
        self.camp_ok(&["create", title, "--run", run]).trim().into()
    }

    fn manifest(&self, run: &str) -> serde_json::Value {
        let p = self.root.join("runs").join(run).join("manifest.json");
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    }

    fn step_bead(&self, run: &str, step: &str) -> String {
        self.manifest(run)["steps"][step]
            .as_str()
            .unwrap()
            .to_owned()
    }

    fn get_bead(&self, id: &str) -> BeadRow {
        camp_core::readiness::get_bead(&self.conn(), id)
            .unwrap()
            .unwrap_or_else(|| panic!("bead {id} not found"))
    }

    fn bead_metadata(&self, id: &str) -> BTreeMap<String, String> {
        camp_core::readiness::bead_metadata(&self.conn(), id).unwrap()
    }

    fn drain_children(&self, anchor: &str) -> BTreeMap<usize, BeadRow> {
        camp_core::formula::runtime::drain_children(&self.conn(), anchor).unwrap()
    }

    /// The set campd would dispatch a WORKER for. No CLI exposes this.
    fn dispatchable(&self) -> Vec<BeadRow> {
        camp_core::readiness::dispatchable_beads(&self.conn()).unwrap()
    }

    fn attempts(&self, run: &str, step: &str, anchor: &str) -> Vec<BeadRow> {
        camp_core::formula::runtime::attempts(&self.conn(), run, step, anchor).unwrap()
    }

    fn events_of_type(&self, t: &str) -> Vec<serde_json::Value> {
        self.camp_ok(&["events", "--json"])
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
            .filter(|e| e["type"] == t)
            .collect()
    }

    /// An item run's ROOT is NEVER closed directly — every root closes via
    /// `flow::finalization`, and `camp close` on a live one hits the same
    /// `InvalidTransition` class as B4. Close the item run's `work` STEP bead and
    /// let campd finalize the root.
    fn close_item(&self, item_root: &str, outcome: &str) {
        let run = self.get_bead(item_root).id;
        let run_id = self
            .conn()
            .query_row("SELECT run_id FROM beads WHERE id = ?1", [&run], |r| {
                r.get::<_, String>(0)
            })
            .unwrap();
        let work = self.step_bead(&run_id, "work");
        self.camp_ok(&["claim", &work, "--session", "s"]);
        self.camp_ok(&["close", &work, "--outcome", outcome]);
    }

    /// Wait until campd has caught up AND has no pending drains — i.e. the
    /// scatter/gather has settled.
    fn settle(&self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            std::thread::sleep(Duration::from_millis(60));
            // campd is idle when its cursor has caught up with the event head.
            let (head, cursor): (i64, i64) = self
                .conn()
                .query_row(
                    "SELECT (SELECT COALESCE(MAX(seq), 0) FROM events),
                            (SELECT COALESCE(MAX(seq), 0) FROM cursors)",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap_or((0, -1));
            if head == cursor {
                // one more beat so an in-flight settle can land its batch
                std::thread::sleep(Duration::from_millis(120));
                let (h2, c2): (i64, i64) = self
                    .conn()
                    .query_row(
                        "SELECT (SELECT COALESCE(MAX(seq), 0) FROM events),
                                (SELECT COALESCE(MAX(seq), 0) FROM cursors)",
                        [],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .unwrap_or((0, -1));
                if h2 == c2 && h2 == head {
                    return;
                }
            }
            assert!(Instant::now() < deadline, "campd never settled");
        }
    }

    /// Close the member-producing step so the drain anchor goes ready.
    fn close_decompose(&self, run: &str) {
        let d = self.step_bead(run, "decompose");
        self.camp_ok(&["claim", &d, "--session", "s"]);
        self.camp_ok(&["close", &d, "--outcome", "pass"]);
    }
}

impl Drop for Camp {
    fn drop(&mut self) {
        self.stop_campd();
    }
}

// ============================================================================

#[test]
fn a_drain_step_creates_NO_ATTEMPT_and_dispatches_NO_WORKER() {
    // ⭐ BD3 — and the assertion is the whole point.
    //
    // `maybe_claim_looping` called `create_attempt` UNCONDITIONALLY after the
    // claim. `create_attempt` emits a `bead.created` with run_id + step_id,
    // `type = task`, OPEN, NO needs — EXACTLY the shape `dispatchable_beads`
    // picks up. So every drain step got a real worker (§13's money invariant),
    // and that phantom attempt's close then closed the anchor early, so the
    // gather's `close_anchor` hit `InvalidTransition` — B4, through B4's own fix.
    //
    // Rev 2's four tests ALL PASSED against that, because they only ever checked
    // THE ANCHOR — and the phantom attempt is a DIFFERENT BEAD ID. So this test
    // asserts on the ATTEMPTS and on the DISPATCHABLE SET, not on the anchor.
    let mut c = Camp::new();
    c.spawn_campd();
    let run = c.sling("build");
    let m1 = c.create_member(&run, "member one");
    c.close_decompose(&run);
    c.settle();

    let anchor = c.step_bead(&run, "implement");

    assert!(
        c.attempts(&run, "implement", &anchor).is_empty(),
        "a drain step has NO attempts — attempts are the check/retry mechanism"
    );
    assert!(
        !c.dispatchable()
            .iter()
            .any(|b| b.id == anchor || b.title.contains("Implement each")),
        "NOTHING carrying the drain step's identity is dispatchable"
    );
    // The anchor is campd's: claimed, in_progress, held.
    let row = c.get_bead(&anchor);
    assert_eq!(row.status, "in_progress");
    assert_eq!(row.claimed_by.as_deref(), Some("campd"));
    // …and the member really did get scattered (so this is not passing by
    // simply doing nothing).
    assert_eq!(c.drain_children(&anchor).len(), 1);
    assert!(
        c.bead_metadata(&m1)
            .contains_key(EXCLUSIVE_DRAIN_RESERVATION)
    );
}

#[test]
fn a_drain_scatters_EVERY_member_in_one_pass() {
    // F7 — EAGER, ALL MEMBERS. gc reserves the whole set and materializes the
    // whole set; there is no throttle and no matrix.
    let mut c = Camp::new();
    c.spawn_campd();
    let run = c.sling("build");
    for i in 0..3 {
        c.create_member(&run, &format!("member {i}"));
    }
    c.close_decompose(&run);
    c.settle();

    let anchor = c.step_bead(&run, "implement");
    let children = c.drain_children(&anchor);
    assert_eq!(
        children.len(),
        3,
        "3 members ⇒ 3 item runs after ONE settle"
    );
}

#[test]
fn an_exclusive_drain_reserves_every_member_with_gcs_verbatim_key() {
    let mut c = Camp::new();
    c.spawn_campd();
    let run = c.sling("build");
    let members: Vec<String> = (0..3)
        .map(|i| c.create_member(&run, &format!("member {i}")))
        .collect();
    c.close_decompose(&run);
    c.settle();

    let anchor = c.step_bead(&run, "implement");
    for m in &members {
        assert_eq!(
            c.bead_metadata(m)
                .get(EXCLUSIVE_DRAIN_RESERVATION)
                .map(String::as_str),
            Some(anchor.as_str()),
            "gc's key VERBATIM, valued with the reserving anchor"
        );
    }
    // No event may ever be named `drain.reserved` — the reservation RIDES
    // `bead.updated`.
    assert!(c.events_of_type("drain.reserved").is_empty());
    assert!(!c.events_of_type("bead.updated").is_empty());
}

#[test]
fn a_conflicting_drain_reserves_NOTHING_and_materializes_NOTHING() {
    // ⭐ BD4 — ALL-OR-NOTHING, and the assertion that rev 2's version lacked.
    //
    // Rev 2 reserved member i BEFORE materializing item i, and on a conflict at
    // k+1 "released 1..k" — while item-run 1 was ALREADY COOKED and its workers
    // dispatchable on m1. m1 then carried NO reservation, so a second drain could
    // reserve it and cook its own item run over it: TWO DRAINS MUTATING ONE BEAD,
    // the exact thing the reservation prevents. Rev 2's test asserted only that
    // the metadata key was gone; it never asserted item-run 1 was not cooked.
    //
    // Here the loser must have cooked NOTHING.
    let mut c = Camp::new();
    c.spawn_campd();
    let run = c.sling("two-drains");
    c.create_member(&run, "contested member");
    c.close_decompose(&run);
    c.settle();

    let a = c.step_bead(&run, "drain-a");
    let b = c.step_bead(&run, "drain-b");
    let (a_kids, b_kids) = (c.drain_children(&a).len(), c.drain_children(&b).len());

    // Exactly one drain won.
    assert!(
        (a_kids == 1 && b_kids == 0) || (a_kids == 0 && b_kids == 1),
        "exactly one drain may materialize: a={a_kids} b={b_kids}"
    );
    let (winner, loser) = if a_kids == 1 { (&a, &b) } else { (&b, &a) };

    // The LOSER materialized NOTHING.
    assert_eq!(c.drain_children(loser).len(), 0);
    // The WINNER still holds the member — the loser's rolled-back batch did not
    // strip it.
    let held = c.bead_metadata(&c.create_member_ids(&run)[0]);
    assert_eq!(
        held.get(EXCLUSIVE_DRAIN_RESERVATION).map(String::as_str),
        Some(winner.as_str()),
        "the winner's reservation survived the loser's rollback"
    );
}

#[test]
fn a_reserve_conflict_closes_the_losing_anchor_and_the_run_FINALIZES() {
    // ⭐ BD5. Emitting `dispatch.failed` alone only appends an event: the
    // campd-held anchor stays `in_progress`, `flow::finalization` returns
    // NotQuiescent FOREVER, and the run never finalizes. The reservation leak
    // would have been traded for a RUN leak.
    let mut c = Camp::new();
    c.spawn_campd();
    let run = c.sling("two-drains");
    c.create_member(&run, "contested member");
    c.close_decompose(&run);
    c.settle();

    let a = c.step_bead(&run, "drain-a");
    let b = c.step_bead(&run, "drain-b");
    let loser = if c.drain_children(&a).is_empty() {
        &a
    } else {
        &b
    };

    let row = c.get_bead(loser);
    assert_eq!(row.status, "closed", "the losing anchor must CLOSE");
    assert_eq!(row.outcome.as_deref(), Some("fail"));
    // …and it says WHY, naming the conflict.
    let failed = c.events_of_type("dispatch.failed");
    assert!(
        failed.iter().any(|e| e["data"]["reason"]
            .as_str()
            .unwrap_or("")
            .contains("conflict")),
        "the failure names the conflict: {failed:?}"
    );
}

#[test]
fn the_reservation_is_released_when_the_drain_gathers() {
    let mut c = Camp::new();
    c.spawn_campd();
    let run = c.sling("build");
    let m1 = c.create_member(&run, "member one");
    c.close_decompose(&run);
    c.settle();

    let anchor = c.step_bead(&run, "implement");
    assert!(
        c.bead_metadata(&m1)
            .contains_key(EXCLUSIVE_DRAIN_RESERVATION)
    );

    let item = c.drain_children(&anchor)[&0].id.clone();
    c.close_item(&item, "pass");
    c.settle();

    assert!(
        !c.bead_metadata(&m1)
            .contains_key(EXCLUSIVE_DRAIN_RESERVATION),
        "the gather releases every member it held"
    );
    let row = c.get_bead(&anchor);
    assert_eq!(row.status, "closed");
    assert_eq!(row.outcome.as_deref(), Some("pass"));
}

#[test]
fn the_run_does_not_finalize_while_drain_items_are_open() {
    // B5 — `flow::finalization` returns NotQuiescent on any in_progress anchor,
    // so the campd-held drain anchor blocks quiescence and every downstream
    // `needs` stays blocked until gather.
    let mut c = Camp::new();
    c.spawn_campd();
    let run = c.sling("build");
    c.create_member(&run, "member one");
    c.close_decompose(&run);
    c.settle();

    assert!(
        c.events_of_type("run.finalized").is_empty(),
        "the run must not finalize while the drain's items are open"
    );
    // `publish` needs `implement`, and `implement` is still open.
    let publish = c.step_bead(&run, "publish");
    assert!(!c.dispatchable().iter().any(|b| b.id == publish));
}

#[test]
fn the_drains_outcome_reflects_a_failed_item_at_gather_and_the_others_still_ran() {
    // F6 — ALWAYS `continue`. An item's failure does not stop the remaining items
    // (they have all already run — the drain is EAGER); the DRAIN's own outcome
    // reflects the failures at gather.
    let mut c = Camp::new();
    c.spawn_campd();
    let run = c.sling("build");
    for i in 0..3 {
        c.create_member(&run, &format!("member {i}"));
    }
    c.close_decompose(&run);
    c.settle();

    let anchor = c.step_bead(&run, "implement");
    let children = c.drain_children(&anchor);
    assert_eq!(children.len(), 3, "all three ran — `continue`, always");

    c.close_item(&children[&0].id, "pass");
    c.close_item(&children[&1].id, "fail");
    c.close_item(&children[&2].id, "pass");
    c.settle();

    let row = c.get_bead(&anchor);
    assert_eq!(row.status, "closed");
    assert_eq!(
        row.outcome.as_deref(),
        Some("fail"),
        "one failed item fails the drain"
    );
}

#[test]
fn a_CLOSED_member_is_never_scattered() {
    // D3 — gc's `Members(includeClosed=false)`. A closed member is finished work;
    // scattering an item run over it would redo it.
    let mut c = Camp::new();
    c.spawn_campd();
    let run = c.sling("build");
    let live = c.create_member(&run, "live member");
    let done = c.create_member(&run, "already done");
    c.camp_ok(&["claim", &done, "--session", "s"]);
    c.camp_ok(&["close", &done, "--outcome", "pass"]);

    c.close_decompose(&run);
    c.settle();

    let anchor = c.step_bead(&run, "implement");
    assert_eq!(c.drain_children(&anchor).len(), 1, "only the LIVE member");
    assert!(
        c.bead_metadata(&live)
            .contains_key(EXCLUSIVE_DRAIN_RESERVATION)
    );
    assert!(
        !c.bead_metadata(&done)
            .contains_key(EXCLUSIVE_DRAIN_RESERVATION)
    );
}

#[test]
fn a_mail_bead_in_a_run_is_never_a_drain_member() {
    // D3 — `type = 'task'`. A mail bead is an open ledger record, not work.
    let mut c = Camp::new();
    c.spawn_campd();
    let run = c.sling("build");
    c.create_member(&run, "real member");
    c.camp_ok(&["create", "a message", "--run", &run, "--type", "mail"]);

    c.close_decompose(&run);
    c.settle();

    let anchor = c.step_bead(&run, "implement");
    assert_eq!(
        c.drain_children(&anchor).len(),
        1,
        "the mail bead is not a member"
    );
}

#[test]
fn a_drain_survives_a_campd_restart_without_double_materializing() {
    let mut c = Camp::new();
    c.spawn_campd();
    let run = c.sling("build");
    c.create_member(&run, "member one");
    c.close_decompose(&run);
    c.settle();

    let anchor = c.step_bead(&run, "implement");
    assert_eq!(c.drain_children(&anchor).len(), 1);

    c.restart_campd();
    c.settle();
    assert_eq!(
        c.drain_children(&anchor).len(),
        1,
        "reconcile re-queues the anchor, and execute_drain GATHERS rather than \
         re-scattering when children already exist"
    );
}

#[test]
fn execute_drain_closes_the_anchor_when_the_item_formula_is_unusable() {
    // The honest test for "a drain whose item formula is missing": it must
    // `dispatch.failed` AND CLOSE THE ANCHOR — never leak the run.
    let mut c = Camp::new();
    c.spawn_campd();
    let run = c.sling("bad-item");
    c.create_member(&run, "member one");
    c.close_decompose(&run);
    c.settle();

    let anchor = c.step_bead(&run, "implement");
    let row = c.get_bead(&anchor);
    assert_eq!(row.status, "closed", "the anchor must close, not leak");
    assert_eq!(row.outcome.as_deref(), Some("fail"));
    assert!(
        c.events_of_type("dispatch.failed")
            .iter()
            .any(|e| e["data"]["reason"]
                .as_str()
                .unwrap_or("")
                .contains("no-such-item-formula")),
        "the failure NAMES the formula"
    );
}

#[test]
fn doctor_lists_and_releases_orphaned_drain_reservations() {
    // The operator escape. A reservation naming an anchor that is CLOSED or GONE
    // is an orphan: no drain will ever gather that member, and no other drain can
    // ever take it.
    let mut c = Camp::new();
    c.spawn_campd();
    let run = c.sling("build");
    let m1 = c.create_member(&run, "member one");
    c.close_decompose(&run);
    c.settle();
    let anchor = c.step_bead(&run, "implement");
    assert!(
        c.bead_metadata(&m1)
            .contains_key(EXCLUSIVE_DRAIN_RESERVATION)
    );

    // Close the anchor out from under the reservation (the kill -9 shape).
    c.stop_campd();
    let item = c.drain_children(&anchor)[&0].id.clone();
    c.close_item(&item, "pass");
    // campd is DOWN, so nothing gathered: the member is still held by an anchor
    // that is about to be closed by hand.
    c.camp_ok(&["close", &anchor, "--outcome", "fail"]);

    let listed = c.camp_ok(&["doctor", "--drain-reservations"]);
    assert!(listed.contains("ORPHAN"), "{listed}");
    assert!(listed.contains(&m1), "{listed}");

    let released = c.camp_ok(&["doctor", "--drain-reservations", "--release-orphans"]);
    assert!(released.contains("released 1"), "{released}");
    assert!(
        !c.bead_metadata(&m1)
            .contains_key(EXCLUSIVE_DRAIN_RESERVATION),
        "the orphan is released"
    );
}

#[test]
fn reconcile_releases_a_reservation_orphaned_by_a_kill_9() {
    // The same orphan, swept AUTOMATICALLY on the next campd start.
    let mut c = Camp::new();
    c.spawn_campd();
    let run = c.sling("build");
    let m1 = c.create_member(&run, "member one");
    c.close_decompose(&run);
    c.settle();
    let anchor = c.step_bead(&run, "implement");

    c.stop_campd();
    let item = c.drain_children(&anchor)[&0].id.clone();
    c.close_item(&item, "pass");
    c.camp_ok(&["close", &anchor, "--outcome", "fail"]);
    assert!(
        c.bead_metadata(&m1)
            .contains_key(EXCLUSIVE_DRAIN_RESERVATION)
    );

    c.spawn_campd(); // reconcile runs on start
    c.settle();
    assert!(
        !c.bead_metadata(&m1)
            .contains_key(EXCLUSIVE_DRAIN_RESERVATION),
        "reconcile sweeps a reservation whose anchor is closed or gone"
    );
}

impl Camp {
    /// The member beads of a run, in creation order.
    fn create_member_ids(&self, run: &str) -> Vec<String> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT id FROM beads
                  WHERE run_id = ?1 AND step_id IS NULL AND type = 'task'
                    AND labels NOT LIKE '%\"drain:%'
                  ORDER BY id",
            )
            .unwrap();
        let root = self.manifest(run)["root"].as_str().unwrap().to_owned();
        stmt.query_map([run], |r| r.get::<_, String>(0))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|id| *id != root)
            .collect()
    }
}

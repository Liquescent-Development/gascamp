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
    /// The agent pack is VALID now (V-5), so campd really does spawn workers. This
    /// gate (`FAKE_AGENT_HOLD_DIR`) makes them CLAIM and then WAIT, so the tests keep
    /// deterministic control of every outcome while the DISPATCH PATH IS GENUINELY
    /// EXERCISED.
    hold: PathBuf,
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
        // V-5: this was `isolation: none` — YAML, in a file parsed as TOML. Every
        // dispatch died with "agent.toml is not valid TOML", so NO WORKER WAS EVER
        // SPAWNED in any drain test, and any "the items really ran" assertion built
        // on this harness was vacuous BY CONSTRUCTION.
        std::fs::write(agent.join("agent.toml"), "isolation = \"none\"\n").unwrap();
        std::fs::write(agent.join("prompt.md"), "do the work\n").unwrap();

        let hold = dir.path().join("hold");
        std::fs::create_dir_all(&hold).unwrap();
        let c = Camp {
            _dir: dir,
            root,
            child: None,
            hold,
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
            .env("FAKE_AGENT_HOLD_DIR", &self.hold)
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

    /// The bead's `step_id` — NOT on `BeadRow` (not in `BEAD_COLS`), so read from the
    /// fold. V-6 needs it: the money-invariant assertion must key on the STEP, not on
    /// a title `create_attempt` happens to copy.
    ///
    /// `None` means the row EXISTS and its `step_id` is NULL — the property that makes
    /// a bead a run MEMBER. A MISSING row is a hard failure, not a `None`: the old
    /// `.unwrap_or(None)` collapsed those two into one answer, and `close_member`'s
    /// guard now rests on this distinction (invariant 5 — no silenced errors).
    fn step_id_of(&self, id: &str) -> Option<String> {
        self.conn()
            .query_row("SELECT step_id FROM beads WHERE id = ?1", [id], |r| {
                r.get::<_, Option<String>>(0)
            })
            .unwrap_or_else(|e| panic!("bead {id} has no row, so it has no step_id: {e}"))
    }

    /// The bead's `run_id`. The OTHER half of the membership predicate — see
    /// `close_member`, which needs BOTH columns because `dispatchable_beads` is a
    /// CONJUNCTION. Same fail-fast contract as `step_id_of`: a missing row is a hard
    /// failure, never a `None`.
    fn run_id_of(&self, id: &str) -> Option<String> {
        self.conn()
            .query_row("SELECT run_id FROM beads WHERE id = ?1", [id], |r| {
                r.get::<_, Option<String>>(0)
            })
            .unwrap_or_else(|e| panic!("bead {id} has no row, so it has no run_id: {e}"))
    }

    /// Every bead campd has woken a worker for, by bead id.
    ///
    /// `session.woke` is campd SAYING WHAT IT DISPATCHED AND TO WHOM. It is the only
    /// evidence in the ledger that a worker ever EXISTED — and it is the evidence the
    /// suite was missing entirely.
    fn woken_beads(&self) -> std::collections::BTreeSet<String> {
        self.events_of_type("session.woke")
            .into_iter()
            .filter_map(|e| e["data"]["bead"].as_str().map(str::to_owned))
            .collect()
    }

    /// ⭐ No dispatch may have FAILED. Called by EVERY test in this file that can call
    /// it — all 14 of them.
    ///
    /// The books BALANCE, and they are meant to be checked: 14 callers + 6 failure-path
    /// tests that must not call it (below) + 5 `should_panic` harness-guard tests
    /// (`close_member_REFUSES_a_dispatched_bead`,
    /// `close_member_REFUSES_a_RUNLESS_dispatched_bead`,
    /// `close_member_REFUSES_a_RUN_SCOPED_MAIL_bead`,
    /// `close_member_REFUSES_a_RUN_ROOT`,
    /// `close_dispatched_REFUSES_a_CLOSED_bead_no_worker_ever_touched`), which panic
    /// INSIDE the harness and can never reach a call site = 25 `#[test]` fns, the whole
    /// file. An earlier version of this comment accounted for 20 of 23 and left the
    /// other 3 unmentioned — in a comment whose entire job is to make every exclusion
    /// explicit. If these three numbers stop summing to the `#[test]` count, an
    /// exclusion is hiding again; that is exactly how F3-B hid.
    ///
    /// The comment here used to SAY "call this on every happy path" while 2 of 20 tests
    /// called it. That is the same class of false in-code claim as a `die()` message
    /// asserting something untrue (invariant 3): the file stated a coverage rule it did
    /// not keep, so a reader trusted a guarantee that was not there. It is now kept, and
    /// what follows is the whole truth about where this runs and what it can see.
    ///
    /// It is NOT called by the six FAILURE-path tests, because they EXPECT a
    /// `dispatch.failed` and assert its reason themselves: `a_conflicting_drain…`,
    /// `a_reserve_conflict…`, `execute_drain_closes_the_anchor…`,
    /// `a_post_reserve_failure…`, `a_failed_drain_does_not_poison…`,
    /// `execute_drain_refuses…`. Injecting this helper into each of those six turns it
    /// RED, so the exclusion is earned, not asserted. `dispatch.failed` is not only a
    /// SPAWN failure — campd also emits it for a reservation conflict and an unusable
    /// item formula, which those six exist to provoke. So this is not a universal
    /// invariant and must not be hoisted into `Drop`.
    ///
    /// F3-B: that list held a SEVENTH name — `a_drain_over_100_members…` — excused on
    /// the same grounds, and for it the grounds were FALSE. It emits no
    /// `dispatch.failed` at all: the cap is checked BEFORE the reserve, so nothing is
    /// dispatched and nothing fails to dispatch (it asserts on a `bead.closed` carrying
    /// `limit_exceeded`). It could always have called this and silently did not. It
    /// calls it now — an exclusion nobody had ever checked, hiding inside the very
    /// comment written to stop unchecked claims.
    ///
    /// And it can only see a dispatch that FAILED LOUDLY. A dispatch that never HAPPENS
    /// logs nothing, and silence is not evidence — that gap is `close_dispatched`'s,
    /// which requires `session.woke` on every accepting path.
    ///
    /// BD-R3-1: the V-5 fix (a valid TOML agent pack) was real and NOTHING asserted
    /// it. Restoring the bug — YAML in a TOML file — left all 20 tests GREEN while
    /// campd spawned ZERO workers and logged THREE `dispatch.failed`. The suite
    /// counted survivors and never asked whether anyone had died. Same survivorship
    /// shape as BD-1 in `e2e_corpus.py`, which I fixed there and left here.
    fn assert_no_dispatch_failures(&self) {
        let failed = self.events_of_type("dispatch.failed");
        assert!(
            failed.is_empty(),
            "campd failed {} dispatch(es) — on a happy path there is no such thing as \
             an acceptable one:\n  {}",
            failed.len(),
            failed
                .iter()
                .map(|e| e["data"]["reason"].as_str().unwrap_or("?").to_owned())
                .collect::<Vec<_>>()
                .join("\n  ")
        );
    }

    /// Close a bead campd DISPATCHED to a worker.
    ///
    /// **There is NO fallback here (invariant 5).** The old `close_bead` would claim
    /// the bead itself if campd had not dispatched it — "both are real states" — which
    /// made the whole suite structurally indifferent to whether a worker ever existed.
    /// That is a harness FALLBACK papering over a real failure, and it is exactly what
    /// let the V-5 mutant pass 20/20.
    ///
    /// A bead that should be dispatched and is not is a HARD FAILURE, and it says so.
    fn close_dispatched(&self, bead: &str, outcome: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let row = self.get_bead(bead);
            match row.status.as_str() {
                // The (held) worker claimed it. `camp close` does not require the
                // claiming session.
                "in_progress" => {
                    assert!(
                        self.woken_beads().contains(bead),
                        "bead {bead} is in_progress but campd never woke a worker for it"
                    );
                    self.camp_ok(&["close", bead, "--outcome", outcome]);
                    return;
                }
                // Already closed — but "no fallback" has to mean EVERY accepting arm,
                // not just the one below. This was the single path by which
                // `close_dispatched` could return with no evidence a worker ever
                // existed, which is precisely the indifference the method was split
                // out to destroy. Same check, same reason.
                "closed" => {
                    assert!(
                        self.woken_beads().contains(bead),
                        "bead {bead} is closed but campd never woke a worker for it — \
                         this method accepts a bead only on POSITIVE evidence a worker \
                         existed (invariant 5), and `session.woke` is that evidence"
                    );
                    return;
                }
                _ => {
                    assert!(
                        Instant::now() < deadline,
                        "bead {bead} was NEVER DISPATCHED (still {:?} after 10s). campd \
                         must spawn a worker for it — if it did not, the agent pack, the \
                         route or the dispatch path is broken, and the harness will NOT \
                         paper over that by claiming the bead itself (invariant 5).\n\
                         dispatch.failed: {:?}",
                        row.status,
                        self.events_of_type("dispatch.failed")
                            .iter()
                            .map(|e| e["data"]["reason"].as_str().unwrap_or("?").to_owned())
                            .collect::<Vec<_>>()
                    );
                    std::thread::sleep(Duration::from_millis(40));
                }
            }
        }
    }

    /// Close a run MEMBER. A member is NEVER dispatched — campd excludes it from
    /// `dispatchable_beads` by design (a DRAIN scatters over it). So the harness
    /// claims it, and that is not a fallback: it is the only path a member has.
    ///
    /// **But it is a fallback the instant it is pointed at the wrong bead**, and until
    /// the guard below existed, nothing stopped that: the choice between this method
    /// and `close_dispatched` was AUTHOR CONVENTION. Both of the absorbing behaviours
    /// just removed from `close_dispatched` still live here — the `in_progress` arm
    /// closes with no `session.woke` check, and the `_` arm CLAIMS AN UNDISPATCHED
    /// BEAD. So calling this on a bead campd should have dispatched silently reinstates
    /// the V-5 blindness in its exact original shape.
    ///
    /// Not theoretical. Delete the member/root exclusion from `dispatchable_beads` and
    /// `a_CLOSED_member_is_never_scattered` STILL PASSES, because this method absorbs
    /// the wrongly-dispatched member. The guard is what turns that into a hard failure.
    ///
    /// So the guard admits ONLY WHAT THE PRODUCT CALLS A MEMBER, and it takes all THREE
    /// of the columns `run_members` keys on (`formula/runtime.rs:600`):
    /// `run_id = ?1 AND step_id IS NULL AND type = 'task'`. This has been widened once
    /// per round because each time it was short, a real bead walked through:
    ///
    /// - round 4 checked `step_id` alone. A RUNLESS bead (`camp create` with no `--run`)
    ///   has `run_id` NULL *and* `step_id` NULL, so the member exclusion in
    ///   `dispatchable_beads` does not fire, campd DISPATCHES it — and it slid straight
    ///   into `close_member`, which absorbed a bead a worker had really run. That is the
    ///   V-5 blindness, reinstated through the check meant to prevent it.
    /// - round 5 added `run_id` and was BLIND TO `type`. A run-scoped MAIL bead has
    ///   `run_id` NOT NULL and `step_id` NULL, so it satisfied both columns and
    ///   `close_member` closed it AS A MEMBER — in the same file as
    ///   `a_mail_bead_in_a_run_is_never_a_drain_member`, which exists to forbid exactly
    ///   that fiction. `type = 'task'` IS D3.
    ///
    /// What the guard is NOT is a mirror of `dispatchable_beads`. That predicate
    /// (`readiness.rs:161-172`) is a SIX-WAY conjunction — `status='open'`,
    /// `type='task'`, `dispatch_failure IS NULL`, the member exclusion, no `sessions`
    /// row, no unmet deps. The member SHAPE is the one conjunct of the six that
    /// GUARANTEES campd will not dispatch the bead, which is what makes accepting only
    /// that shape SOUND: nothing campd wakes can get in here. It is not EXHAUSTIVE and
    /// must not claim to be — a memory bead, a mail bead or a task with an unmet
    /// `--needs` is also undispatchable, and is also rejected here, because this method
    /// is for MEMBERS, not merely for undispatched beads. So the panic does not tell
    /// such an author that campd dispatches their bead; an earlier version did, and it
    /// would have sent them to `close_dispatched` to spin ten seconds and hard-fail on
    /// "NEVER DISPATCHED".
    ///
    /// …and the ROOT is excluded too, because `run_members` excludes it — by ID
    /// (`b.id <> ?2`), the fourth condition of that predicate. This was NOT theoretical
    /// either: a run root has `run_id` SET, `step_id` NULL and `type = 'task'`, so it
    /// wears the member shape EXACTLY, and a probe routing one here found `close_member`
    /// CLAIMED AND CLOSED IT with no panic. The docstring of this very method briefly
    /// claimed a root would hard-fail on `InvalidTransition` instead; that was measured
    /// and it was false. Excluding by id is what `run_members` does, so it is what this
    /// does. It also catches a COOKED run root (`bond:`/`drain:` labelled), which is the
    /// root of its own run and therefore its own `manifest.root`.
    ///
    /// One condition of `run_members` is deliberately NOT asserted: `status <> 'closed'`.
    /// A CLOSED member is still a member and this method must accept one — its `closed`
    /// arm returns, and `a_CLOSED_member_is_never_scattered` closes one through here on
    /// purpose.
    ///
    /// `close_member_REFUSES_a_dispatched_bead`,
    /// `close_member_REFUSES_a_RUNLESS_dispatched_bead`,
    /// `close_member_REFUSES_a_RUN_SCOPED_MAIL_bead` and
    /// `close_member_REFUSES_a_RUN_ROOT` hold this shut.
    fn close_member(&self, bead: &str, outcome: &str) {
        let run_id = self.run_id_of(bead);
        let step_id = self.step_id_of(bead);
        let kind = self.get_bead(bead).kind;
        // `run_members`' `b.id <> ?2`: the root wears the member shape exactly.
        let is_root = run_id
            .as_deref()
            .is_some_and(|r| self.manifest(r)["root"].as_str() == Some(bead));
        assert!(
            run_id.is_some() && step_id.is_none() && kind == "task" && !is_root,
            "close_member called on a bead that is NOT a run member ({bead}: run_id={:?} \
             step_id={:?} type={kind:?} is_root={is_root}) — the product's member \
             predicate is `run_id = ? AND step_id IS NULL AND type = 'task'` AND \
             `id <> <root>` (formula/runtime.rs:600), and this method is ONLY for \
             members. If campd dispatches this bead, use close_dispatched; if it does \
             not, the bead does not belong in this suite",
            run_id,
            step_id
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let row = self.get_bead(bead);
            match row.status.as_str() {
                "closed" => return,
                "in_progress" => {
                    self.camp_ok(&["close", bead, "--outcome", outcome]);
                    return;
                }
                _ => {
                    let out = self.camp(&["claim", bead, "--session", "harness"]);
                    if out.status.success() {
                        self.camp_ok(&["close", bead, "--outcome", outcome]);
                        return;
                    }
                }
            }
            assert!(
                Instant::now() < deadline,
                "member {bead} never became closable"
            );
            std::thread::sleep(Duration::from_millis(40));
        }
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
        // An item run's `work` bead IS dispatched — that is the whole point of a
        // drain, and it is the fact the suite never checked.
        self.close_dispatched(&work, outcome);
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
        self.close_dispatched(&d, "pass");
    }
}

impl Drop for Camp {
    fn drop(&mut self) {
        // Release every held worker so none lingers past the test.
        if let Ok(mut s) = self.conn().prepare("SELECT id FROM beads")
            && let Ok(rows) = s.query_map([], |r| r.get::<_, String>(0))
        {
            for id in rows.filter_map(Result::ok) {
                let _ = std::fs::write(self.hold.join(id), "go");
            }
        }
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
    // A drain step spawns no worker for its ANCHOR — but the rest of the run must
    // still dispatch normally. Zero failures (BD-R3-1).
    c.assert_no_dispatch_failures();

    assert!(
        c.attempts(&run, "implement", &anchor).is_empty(),
        "a drain step has NO attempts — attempts are the check/retry mechanism"
    );
    assert!(
        !c.dispatchable()
            .iter()
            .any(|b| c.step_id_of(&b.id).as_deref() == Some("implement")),
        "NOTHING carrying the drain step's STEP_ID is dispatchable (V-6: on the \
         step_id, not a title `create_attempt` happens to copy)"
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

    // ⭐ BD-R3-1 — THE WORK ACTUALLY RAN. A drain that materializes item runs nobody
    // ever works is a drain-shaped no-op, and the suite could not tell the difference:
    // with the V-5 agent-pack bug restored, campd spawned ZERO workers and all 20
    // tests still passed.
    c.assert_no_dispatch_failures();
    let woken = c.woken_beads();
    for (index, root) in &children {
        let run_id = c
            .conn()
            .query_row("SELECT run_id FROM beads WHERE id = ?1", [&root.id], |r| {
                r.get::<_, String>(0)
            })
            .unwrap();
        let work = c.step_bead(&run_id, "work");
        assert!(
            woken.contains(&work),
            "item {index}: campd never WOKE A WORKER for its work bead {work} — the \
             drain scattered a run nobody works. session.woke: {woken:?}"
        );
    }

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
    c.assert_no_dispatch_failures();
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
    // V-1: TWO members. With ONE, a single-event `append_batch` and rev-2's
    // incremental per-event loop are INDISTINGUISHABLE — BD4's mutant survived the
    // ENTIRE suite because this fixture could not reach the regime where it differs.
    let run = c.sling("two-drains");
    c.create_member(&run, "contested A");
    c.create_member(&run, "contested B");
    c.close_decompose(&run);
    c.settle();

    let a = c.step_bead(&run, "drain-a");
    let b = c.step_bead(&run, "drain-b");
    let (a_kids, b_kids) = (c.drain_children(&a).len(), c.drain_children(&b).len());

    // Exactly one drain won — and it took BOTH members.
    assert!(
        (a_kids == 2 && b_kids == 0) || (a_kids == 0 && b_kids == 2),
        "exactly one drain may materialize, and it materializes EVERY member: \
         a={a_kids} b={b_kids}"
    );
    let (winner, loser) = if a_kids == 2 { (&a, &b) } else { (&b, &a) };

    // The LOSER materialized NOTHING — not "some of it".
    assert_eq!(c.drain_children(loser).len(), 0);
    // …and holds NOTHING. Under the incremental shape the loser would have taken
    // member[0] before conflicting on member[1], and there would be no rollback.
    for m in c.create_member_ids(&run) {
        assert_eq!(
            c.bead_metadata(&m)
                .get(EXCLUSIVE_DRAIN_RESERVATION)
                .map(String::as_str),
            Some(winner.as_str()),
            "every member is held by the WINNER; the loser's batch rolled back WHOLE"
        );
    }
}

#[test]
fn a_reserve_conflict_closes_the_losing_anchor_and_the_run_FINALIZES() {
    // ⭐ BD5. Emitting `dispatch.failed` alone only appends an event: the
    // campd-held anchor stays `in_progress`, `flow::finalization` returns
    // NotQuiescent FOREVER, and the run never finalizes. The reservation leak
    // would have been traded for a RUN leak.
    let mut c = Camp::new();
    c.spawn_campd();
    // V-1: TWO members. With ONE, a single-event `append_batch` and rev-2's
    // incremental per-event loop are INDISTINGUISHABLE — BD4's mutant survived the
    // ENTIRE suite because this fixture could not reach the regime where it differs.
    let run = c.sling("two-drains");
    c.create_member(&run, "contested A");
    c.create_member(&run, "contested B");
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
    c.assert_no_dispatch_failures();
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
    c.assert_no_dispatch_failures();
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
    // An item that FAILS is a bead OUTCOME, not a dispatch failure: all three items
    // were dispatched to real workers, and every one of them must have been.
    c.assert_no_dispatch_failures();
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
    c.close_member(&done, "pass");

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
    c.assert_no_dispatch_failures();
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
    c.assert_no_dispatch_failures();
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
    // The restart must not have cost a dispatch either.
    c.assert_no_dispatch_failures();
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
    // The anchor was closed BY HAND, not by a failed dispatch — campd dispatched
    // everything it owed.
    c.assert_no_dispatch_failures();
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
    c.assert_no_dispatch_failures();
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

// ---- the review's gaps ----------------------------------------------------

#[test]
fn each_item_run_NAMES_ITS_OWN_MEMBER_and_two_items_are_distinguishable() {
    // ⭐ BD-3. The drain used to scatter BYTE-IDENTICAL CLONES: nothing on an item
    // run named its member, so a worker dispatched for it could not know WHICH
    // member it was meant to work. The correspondence existed only as a positional
    // index into `run_members` — never persisted, and not even stable.
    //
    // gc answers this and camp reproduces gc: the member is BOUND into the item
    // formula's vars (`{{issue}}`, gc's LegacyIssueVar) and STAMPED on the item
    // root (`gc.drain_member_id`, gc's key verbatim).
    //
    // This test was IMPOSSIBLE to write against the old code. That was the tell.
    let mut c = Camp::new();
    c.spawn_campd();
    let run = c.sling("build");
    let m0 = c.create_member(&run, "member ALPHA");
    let m1 = c.create_member(&run, "member BETA");
    c.close_decompose(&run);
    c.settle();

    let anchor = c.step_bead(&run, "implement");
    let children = c.drain_children(&anchor);
    assert_eq!(children.len(), 2);

    // Each item root NAMES its own member, with gc's keys.
    let mut named: Vec<String> = Vec::new();
    for (index, root) in &children {
        let md = c.bead_metadata(&root.id);
        let member = md
            .get("gc.drain_member_id")
            .unwrap_or_else(|| panic!("item {index} does not name its member: {md:?}"))
            .clone();
        assert_eq!(
            md.get("gc.drain_control_id").map(String::as_str),
            Some(anchor.as_str())
        );
        assert_eq!(
            md.get("gc.drain_index").map(String::as_str),
            Some(index.to_string().as_str())
        );
        assert_eq!(md.get("gc.drain_count").map(String::as_str), Some("2"));
        assert_eq!(
            md.get("gc.drain_member_access").map(String::as_str),
            Some("exclusive")
        );
        named.push(member);
    }
    named.sort();
    let mut want = vec![m0.clone(), m1.clone()];
    want.sort();
    assert_eq!(
        named, want,
        "the two item runs name the two DISTINCT members"
    );

    // …and the member is BOUND INTO THE WORK, so the item worker's own bead says
    // which member it is working. The two item runs are DISTINGUISHABLE.
    let mut titles: Vec<String> = Vec::new();
    for root in children.values() {
        let run_id = c
            .conn()
            .query_row("SELECT run_id FROM beads WHERE id = ?1", [&root.id], |r| {
                r.get::<_, String>(0)
            })
            .unwrap();
        let work = c.step_bead(&run_id, "work");
        titles.push(c.get_bead(&work).title);
    }
    titles.sort();
    assert_eq!(
        titles,
        vec![format!("Work member {m0}"), format!("Work member {m1}")]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>(),
        "each item's WORK bead names its own member — the runs are not clones"
    );
    c.assert_no_dispatch_failures();
}

#[test]
fn a_post_reserve_failure_RELEASES_every_member_it_held() {
    // ⭐ BD-1. `execute_drain` reserves the WHOLE member set and only THEN resolves
    // the rig, compiles the item formula, checks runnability and cooks. Every one of
    // those failures used to close the anchor and release NOTHING — so a plain
    // MISSING ITEM FORMULA leaked the reservation WITH CAMPD ALIVE AND HEALTHY, and
    // `reconcile` (the only automatic sweep) runs ONCE, AT STARTUP.
    let mut c = Camp::new();
    c.spawn_campd();
    let run = c.sling("bad-item");
    let m = c.create_member(&run, "member one");
    c.close_decompose(&run);
    c.settle();

    let anchor = c.step_bead(&run, "implement");
    let row = c.get_bead(&anchor);
    assert_eq!(row.status, "closed");
    assert_eq!(row.outcome.as_deref(), Some("fail"));

    // THE POINT: the reservation is GONE, with campd still running.
    assert!(
        !c.bead_metadata(&m)
            .contains_key(EXCLUSIVE_DRAIN_RESERVATION),
        "a failed drain must release what it reserved — campd is alive, and nothing \
         else will sweep this until the next restart"
    );
    // …and `doctor` agrees there is nothing orphaned.
    let listed = c.camp_ok(&["doctor", "--drain-reservations"]);
    assert!(listed.contains("no orphaned"), "{listed}");
}

#[test]
fn a_failed_drain_does_not_poison_a_HEALTHY_SIBLING_drain() {
    // ⭐ BD-2. The leaked reservation used to hard-fail a sibling drain whose item
    // formula was FINE — its member was "held" by a CLOSED anchor that would never
    // gather anything — and the `dispatch.failed` asserted "two drains must never
    // mutate one bead" WHEN ONLY ONE DRAIN WAS LIVE. Invariant 3: the event named a
    // cause that was not true.
    let mut c = Camp::new();
    c.spawn_campd();
    let run = c.sling("mixed-drains");
    let m = c.create_member(&run, "member one");
    c.close_decompose(&run);
    c.settle();

    let bad = c.step_bead(&run, "drain-bad");
    let good = c.step_bead(&run, "drain-good");

    // The broken drain fails — and releases.
    assert_eq!(c.get_bead(&bad).outcome.as_deref(), Some("fail"));

    // ⭐ The HEALTHY sibling ran: it scattered its member and holds it.
    assert_eq!(
        c.drain_children(&good).len(),
        1,
        "the healthy drain's item formula is fine and its member is free — it must \
         scatter, not be poisoned by the other drain's leak"
    );
    assert_eq!(
        c.bead_metadata(&m)
            .get(EXCLUSIVE_DRAIN_RESERVATION)
            .map(String::as_str),
        Some(good.as_str()),
        "the LIVE drain holds the member, not the dead one"
    );
    // …and no event claims a conflict that never happened.
    for e in c.events_of_type("dispatch.failed") {
        let reason = e["data"]["reason"].as_str().unwrap_or("");
        assert!(
            !reason.contains("already reserved"),
            "a `dispatch.failed` naming a reservation conflict when only ONE drain is \
             live names a cause that is not true (invariant 3): {reason}"
        );
    }
}

#[test]
fn a_member_that_CLOSES_MID_DRAIN_is_still_released_at_gather() {
    // V-4. The gather's release loop used to iterate `run_members`, which filters
    // `status <> 'closed'` — so a member that closed while its item run was in
    // flight was SKIPPED and kept its reservation forever. Releases now ask
    // `bead_meta`, which is status-agnostic.
    //
    // Reachable only because BD-3 is fixed: the item worker now has a handle on its
    // member.
    let mut c = Camp::new();
    c.spawn_campd();
    let run = c.sling("build");
    let m = c.create_member(&run, "member one");
    c.close_decompose(&run);
    c.settle();

    let anchor = c.step_bead(&run, "implement");
    assert!(
        c.bead_metadata(&m)
            .contains_key(EXCLUSIVE_DRAIN_RESERVATION)
    );

    // The member CLOSES while its item run is still open.
    c.close_member(&m, "pass");
    c.settle();
    assert_eq!(c.get_bead(&m).status, "closed");

    let item = c.drain_children(&anchor)[&0].id.clone();
    c.close_item(&item, "pass");
    c.settle();

    assert!(
        !c.bead_metadata(&m)
            .contains_key(EXCLUSIVE_DRAIN_RESERVATION),
        "a member that closed mid-drain must still be released at gather"
    );
    c.assert_no_dispatch_failures();
}

#[test]
fn execute_drain_refuses_a_not_runnable_item_formula() {
    // V-3 — D1's THIRD cook entry point, and §13's MONEY INVARIANT on the very path
    // this phase added to guard it.
    //
    // The old test used a formula whose NAME did not resolve, which exercises the
    // `compile_named` Err arm — NOT the `not_runnable` arm. The whole `not_runnable`
    // guard could be deleted and the suite stayed green, while a not-runnable item
    // formula was cooked and dispatched to real workers.
    //
    // `no-contract-item` COMPILES fine. It is IMPORTED and declares no graph
    // compiler, so D1 (ruling E) refuses it at RUN time.
    let mut c = Camp::new();
    c.spawn_campd();
    let run = c.sling("not-runnable-drain");
    let m = c.create_member(&run, "member one");
    c.close_decompose(&run);
    c.settle();

    let anchor = c.step_bead(&run, "implement");
    assert_eq!(c.get_bead(&anchor).outcome.as_deref(), Some("fail"));
    assert_eq!(c.drain_children(&anchor).len(), 0, "NOTHING may be cooked");
    assert!(
        c.events_of_type("dispatch.failed").iter().any(|e| {
            let r = e["data"]["reason"].as_str().unwrap_or("");
            r.contains("cannot be run") && r.contains("no-contract-item")
        }),
        "the refusal must name the formula AND say it cannot be RUN (not that it \
         failed to compile — it compiles fine)"
    );
    // BD-1: and it released.
    assert!(
        !c.bead_metadata(&m)
            .contains_key(EXCLUSIVE_DRAIN_RESERVATION)
    );
}

#[test]
fn a_drain_over_100_members_fails_the_drain_and_scatters_nothing() {
    // V-2 — gc's runtime cap (`defaultDrainMaxUnits`, drain.go:24). Correct today,
    // and UNDEFENDED: `flow::DRAIN_MAX_UNITS` and the `>` boundary could both change
    // with a green suite.
    let mut c = Camp::new();
    c.spawn_campd();
    let run = c.sling("build");
    let members: Vec<String> = (0..101)
        .map(|i| c.create_member(&run, &format!("member {i}")))
        .collect();
    c.close_decompose(&run);
    c.settle();

    let anchor = c.step_bead(&run, "implement");
    let row = c.get_bead(&anchor);
    assert_eq!(row.status, "closed");
    assert_eq!(row.outcome.as_deref(), Some("fail"));
    assert_eq!(
        c.drain_children(&anchor).len(),
        0,
        "over the cap, the drain scatters NOTHING — camp will not spawn 101 workers \
         where gc hard-fails"
    );
    // Nothing was reserved: the cap is checked BEFORE the reserve.
    for m in &members {
        assert!(!c.bead_metadata(m).contains_key(EXCLUSIVE_DRAIN_RESERVATION));
    }
    assert!(
        c.events_of_type("bead.closed")
            .iter()
            .any(|e| e["data"]["reason"]
                .as_str()
                .unwrap_or("")
                .contains("limit_exceeded")),
        "the close names gc's reason"
    );
    // F3-B: this test emits NO `dispatch.failed` at all — the cap is checked BEFORE the
    // reserve, so nothing is ever dispatched and nothing ever fails to dispatch. It was
    // excluded from this helper for a reason that was simply untrue.
    c.assert_no_dispatch_failures();
}

#[test]
#[should_panic(expected = "use close_dispatched")]
fn close_member_REFUSES_a_dispatched_bead() {
    // ⭐ F1 — the harness's OWN routing is now a checked contract, not a convention.
    //
    // `close_member` must claim an unclaimed bead and close an in_progress one with no
    // `session.woke` check — a member has no worker, so that is its only path. Pointed
    // at a bead campd SHOULD have dispatched, those same two arms become exactly the
    // fallback that `close_dispatched` was split out to destroy, and the suite goes
    // blind in the V-5 shape again. Nothing but author convention prevented that.
    //
    // `decompose` is a STEP bead: campd dispatches it, and every other happy path here
    // routes it to `close_dispatched` (via `close_decompose`). Sending it to
    // `close_member` must be a HARD FAILURE.
    //
    // This test is the reason the guard cannot be quietly deleted: remove the assert
    // and `close_member` absorbs the dispatched bead, no panic, and this goes RED.
    let mut c = Camp::new();
    c.spawn_campd();
    let run = c.sling("build");
    c.create_member(&run, "member one");
    // The bead must EXIST before its kind can be read — `step_id_of` hard-fails on a
    // missing row rather than reporting a NULL `step_id`, which would look like a
    // member and let the wrong-kind call through.
    c.settle();

    let decompose = c.step_bead(&run, "decompose");
    assert_eq!(
        c.step_id_of(&decompose).as_deref(),
        Some("decompose"),
        "precondition: this IS a dispatched step bead"
    );
    c.close_member(&decompose, "pass");
}

#[test]
#[should_panic(expected = "is NOT a run member")]
fn close_member_REFUSES_a_RUNLESS_dispatched_bead() {
    // ⭐ F1-B. Round 4's guard asserted only `step_id IS NULL`, and was SOLD as
    // mirroring `dispatchable_beads`. It did not. That predicate is a CONJUNCTION —
    // `NOT (run_id IS NOT NULL AND step_id IS NULL)` — so a NULL `step_id` is merely
    // NECESSARY for membership, never sufficient, and the guard tested one conjunct.
    //
    // A RUNLESS bead is the counter-example it let through: `camp create` with no
    // `--run` leaves `run_id` NULL *and* `step_id` NULL, so the exclusion evaluates
    // `NOT(false AND true)` = TRUE — it is DISPATCHABLE, campd really wakes a worker
    // for it, and yet it satisfied the one-conjunct guard. `close_member` then absorbed
    // a bead a worker had genuinely run: the V-5 blindness reinstated in its exact
    // original shape, straight through the check that exists to prevent it.
    //
    // Nothing in the suite was blind (it creates no runless beads) — but a guard sold
    // as making wrong-kind routing IMPOSSIBLE has to actually do that. This test is why
    // the second conjunct cannot be dropped again.
    let mut c = Camp::new();
    c.spawn_campd();
    let bead: String = c.camp_ok(&["create", "a runless bead"]).trim().into();
    c.settle();

    // Precisely the shape the old guard could not see.
    assert_eq!(c.run_id_of(&bead), None, "precondition: belongs to NO run");
    assert_eq!(
        c.step_id_of(&bead),
        None,
        "precondition: NULL step_id — this is what satisfied round 4's guard"
    );
    assert!(
        c.woken_beads().contains(&bead),
        "precondition: campd DISPATCHED it — a runless bead is not excluded by \
         `dispatchable_beads`, so a worker really ran it"
    );

    c.close_member(&bead, "pass");
}

#[test]
#[should_panic(expected = "is NOT a run member")]
fn close_member_REFUSES_a_RUN_SCOPED_MAIL_bead() {
    // ⭐ F8. The guard read `run_id` and `step_id` and was BLIND TO `type`.
    //
    // The product's member predicate (`formula/runtime.rs:600`) is
    // `run_id = ?1 AND step_id IS NULL AND type = 'task'` — THREE columns. Round 4's
    // guard had one, round 5 widened it to two, and a run-scoped MAIL bead satisfies
    // both of those: `run_id` NOT NULL, `step_id` NULL. It sailed through, and
    // `close_member` closed it AS A MEMBER.
    //
    // This file already creates exactly this bead 480 lines up, inside a test named
    // `a_mail_bead_in_a_run_is_never_a_drain_member` — so the harness stood ready to
    // call a mail bead a member in the same file that forbids it. `type` is not a
    // detail of the member predicate; it is the whole of D3.
    let mut c = Camp::new();
    c.spawn_campd();
    let run = c.sling("build");
    let mail: String = c
        .camp_ok(&["create", "a message", "--run", &run, "--type", "mail"])
        .trim()
        .into();
    c.settle();

    // The exact shape that defeated the two-column guard.
    assert!(
        c.run_id_of(&mail).is_some(),
        "precondition: scoped to the run"
    );
    assert_eq!(c.step_id_of(&mail), None, "precondition: NULL step_id");
    assert_eq!(c.get_bead(&mail).kind, "mail", "precondition: NOT a task");
    assert!(
        !c.woken_beads().contains(&mail),
        "precondition: campd never woke it — a mail bead is an open ledger record, \
         not work"
    );

    c.close_member(&mail, "pass");
}

#[test]
#[should_panic(expected = "is NOT a run member")]
fn close_member_REFUSES_a_RUN_ROOT() {
    // ⭐ F8, second hole — found by refusing to SAY a thing I had not MEASURED.
    //
    // While fixing the type-blindness I wrote in the docstring that a run ROOT routed
    // here could not be silently absorbed, because `camp close` on a live root hits
    // `InvalidTransition`. Then I probed it instead of shipping it, and it was FALSE:
    // the root has `run_id` SET, `step_id` NULL and `type = 'task'` — the member shape,
    // exactly — so it passed the widened guard, and `close_member` CLAIMED AND CLOSED
    // IT. No panic. The harness would have called a run root a drain member.
    //
    // `run_members` excludes the root by ID (`b.id <> ?2`, formula/runtime.rs:600). So
    // does the guard now. This test is why that cannot be dropped.
    let mut c = Camp::new();
    c.spawn_campd();
    let run = c.sling("build");
    c.create_member(&run, "member one");
    c.settle();

    let root = c.manifest(&run)["root"].as_str().unwrap().to_owned();
    // The root wears the member shape in every column the guard reads.
    assert!(c.run_id_of(&root).is_some(), "precondition: run_id SET");
    assert_eq!(c.step_id_of(&root), None, "precondition: NULL step_id");
    assert_eq!(c.get_bead(&root).kind, "task", "precondition: type = task");

    c.close_member(&root, "pass");
}

#[test]
#[should_panic(expected = "is closed but campd never woke")]
fn close_dispatched_REFUSES_a_CLOSED_bead_no_worker_ever_touched() {
    // ⭐ F2. `close_dispatched` promises it accepts a bead ONLY on positive evidence a
    // worker existed — that promise is the entire reason it was split out of
    // `close_member`. Its `closed` arm used to `return` unconditionally: the one
    // accepting path that took the bead's word for it, while the adjacent `in_progress`
    // arm demanded `session.woke`.
    //
    // Measured, not assumed: with a `panic!` as the first statement of that arm the
    // whole suite still passed, so NO existing test reaches it — the arm was dead code
    // and an assert dropped into it would have been an instrument nothing exercises.
    // This test is what makes the arm live and the assert falsifiable.
    //
    // A run MEMBER is the honest witness. campd never dispatches one, so it carries no
    // `session.woke` — close it as a member and it becomes exactly the thing the arm
    // must refuse: a bead that is CLOSED but was NEVER WORKED. Restore the bare
    // `"closed" => return` and `close_dispatched` swallows it, no panic, and this
    // goes RED.
    let mut c = Camp::new();
    c.spawn_campd();
    let run = c.sling("build");
    let m = c.create_member(&run, "member one");
    c.settle();

    c.close_member(&m, "pass");
    assert_eq!(c.get_bead(&m).status, "closed");
    assert!(
        !c.woken_beads().contains(&m),
        "precondition: a member is never dispatched, so no worker ever ran it"
    );

    c.close_dispatched(&m, "pass");
}

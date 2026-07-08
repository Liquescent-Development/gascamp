#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Phase 9 integration (master plan test obligations; spec §8.2, §8.3):
//! every formula construct executing with gc semantics against a REAL
//! campd, driven by fake-agent.sh — diamond fan-out, check loops, retry
//! classification, on_complete bonds, run finalization, and kill -9
//! self-healing. `doctor --refold` is asserted clean after every run
//! (the master-plan exit criterion, literally).
//!
//! Test-side waiting polls the ledger — sanctioned for harnesses only;
//! campd itself never polls.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_camp");
const READY_PREFIX: &str = "campd listening on ";

fn fake_agent() -> String {
    format!("{}/tests/fake-agent.sh", env!("CARGO_MANIFEST_DIR"))
}

fn camp(root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .env_remove("CAMP_DIR")
        .arg("--camp")
        .arg(root)
        .args(args)
        .output()
        .unwrap()
}

fn camp_ok(root: &Path, args: &[&str]) -> String {
    let out = camp(root, args);
    assert!(
        out.status.success(),
        "camp {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// A camp with one rig, fake-agent dispatch, and a formula. Returns
/// (root, rig).
fn scaffold(dir: &Path, max_workers: usize) -> (PathBuf, PathBuf) {
    let root = dir.join(".camp");
    std::fs::create_dir_all(&root).unwrap();
    let rig = dir.join("repo");
    std::fs::create_dir_all(&rig).unwrap();
    std::fs::write(
        root.join("camp.toml"),
        format!(
            "[camp]\nname = \"t\"\n\n[[rigs]]\nname = \"gc\"\npath = \"{}\"\nprefix = \"gc\"\n\n\
             [dispatch]\nmax_workers = {max_workers}\ncommand = \"{}\"\ndefault_agent = \"dev\"\n",
            rig.display(),
            fake_agent(),
        ),
    )
    .unwrap();
    let agents = root.join("agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(agents.join("dev.md"), "---\nname: dev\n---\nDo the work.\n").unwrap();
    camp_ok(&root, &["events", "--json"]); // create the ledger
    (root, rig)
}

fn write_formula(root: &Path, name: &str, toml: &str) {
    std::fs::create_dir_all(root.join("formulas")).unwrap();
    std::fs::write(root.join("formulas").join(format!("{name}.toml")), toml).unwrap();
}

fn write_script(rig: &Path, name: &str, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let path = rig.join(name);
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn events_json(root: &Path) -> Vec<serde_json::Value> {
    camp_ok(root, &["events", "--json"])
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// Test-harness wait: poll the ledger until `pred` or panic with the dump.
fn wait_until(root: &Path, what: &str, pred: impl Fn(&[serde_json::Value]) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let events = events_json(root);
        if pred(&events) {
            return;
        }
        if Instant::now() > deadline {
            panic!("timed out waiting for {what}; events: {events:#?}");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn count(events: &[serde_json::Value], kind: &str) -> usize {
    events.iter().filter(|e| e["type"] == kind).count()
}

fn seq_of(events: &[serde_json::Value], pred: impl Fn(&serde_json::Value) -> bool) -> i64 {
    events
        .iter()
        .find(|e| pred(e))
        .unwrap_or_else(|| panic!("event not found in {events:#?}"))["seq"]
        .as_i64()
        .unwrap()
}

fn finalized_of(events: &[serde_json::Value], root_bead: &str) -> serde_json::Value {
    events
        .iter()
        .find(|e| e["type"] == "run.finalized" && e["data"]["root"] == root_bead)
        .unwrap_or_else(|| panic!("no run.finalized for {root_bead} in {events:#?}"))["data"]
        .clone()
}

/// The master-plan exit criterion, asserted literally after every run.
fn assert_refold_clean(root: &Path) {
    let out = camp_ok(root, &["doctor", "--refold"]);
    assert!(out.contains("0 drift rows"), "doctor --refold said: {out}");
}

/// `camp sling --formula <name>` -> (run_id, root bead).
fn sling_formula(root: &Path, name: &str) -> (String, String) {
    let out = camp_ok(root, &["sling", "--formula", name]);
    let mut words = out.split_whitespace();
    let run_id = words.next().unwrap().to_owned();
    assert_eq!(words.next(), Some("root"));
    (run_id, words.next().unwrap().to_owned())
}

struct Daemon {
    child: Child,
}

impl Daemon {
    fn spawn(root: &Path, envs: &[(&str, &str)]) -> Daemon {
        let mut cmd = Command::new(BIN);
        cmd.env_remove("CAMP_DIR")
            .env("CAMP_BIN", BIN)
            .args(["daemon", "--camp"])
            .arg(root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        for (k, v) in envs {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut line = String::new();
        BufReader::new(stdout).read_line(&mut line).unwrap();
        assert!(
            line.starts_with(READY_PREFIX),
            "unexpected first line from campd: {line:?}"
        );
        Daemon { child }
    }

    /// kill -9, the supported shutdown method (invariant 3).
    fn kill9(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        std::mem::forget(self); // Drop would double-kill
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

const DIAMOND: &str = "formula = \"diamond4\"\n\n[[steps]]\nid = \"design\"\ntitle = \"Design\"\n\n\
    [[steps]]\nid = \"implement\"\ntitle = \"Implement\"\nneeds = [\"design\"]\n\n\
    [[steps]]\nid = \"document\"\ntitle = \"Document\"\nneeds = [\"design\"]\n\n\
    [[steps]]\nid = \"release\"\ntitle = \"Release\"\nneeds = [\"implement\", \"document\"]\n";

/// Master plan: "diamond fan-out runs to completion" + the
/// dispatch-latency FUNCTIONAL assertion (close -> dependent dispatch
/// observed in ledger order; the wall-clock number is Phase 13/15).
#[test]
fn diamond_runs_to_completion_and_dependents_dispatch_after_their_needs_close() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig) = scaffold(dir.path(), 10);
    write_formula(&root, "diamond4", DIAMOND);
    let _campd = Daemon::spawn(&root, &[]);

    let (run_id, root_bead) = sling_formula(&root, "diamond4");
    wait_until(&root, "the diamond to finalize", |e| {
        count(e, "run.finalized") == 1
    });
    let events = events_json(&root);

    // every step closed pass; the root closed pass; the verdict is pass/pass
    let cooked = events.iter().find(|e| e["type"] == "run.cooked").unwrap()["data"].clone();
    assert_eq!(cooked["run_id"], run_id.as_str());
    let step_bead = |id: &str| cooked["steps"][id].as_str().unwrap().to_owned();
    for id in ["design", "implement", "document", "release"] {
        let bead = step_bead(id);
        let close = events
            .iter()
            .find(|e| e["type"] == "bead.closed" && e["bead"] == bead.as_str())
            .unwrap_or_else(|| panic!("{id} never closed"));
        assert_eq!(close["data"]["outcome"], "pass", "{id}");
    }
    let finalized = finalized_of(&events, &root_bead);
    assert_eq!(finalized["outcome"], "pass");
    assert_eq!(finalized["final_disposition"], "pass");

    // the FUNCTIONAL latency assertion: a dependent's worker wakes only
    // after its needs closed, in the very ledger order (spec §7.3/§8.3)
    let woke_seq = |bead: &str| {
        seq_of(&events, |e| {
            e["type"] == "session.woke" && e["data"]["bead"] == bead
        })
    };
    let close_seq =
        |bead: &str| seq_of(&events, |e| e["type"] == "bead.closed" && e["bead"] == bead);
    assert!(woke_seq(&step_bead("implement")) > close_seq(&step_bead("design")));
    assert!(woke_seq(&step_bead("document")) > close_seq(&step_bead("design")));
    assert!(woke_seq(&step_bead("release")) > close_seq(&step_bead("implement")));
    assert!(woke_seq(&step_bead("release")) > close_seq(&step_bead("document")));

    assert_refold_clean(&root);
}

const CHECKED: &str = "formula = \"checked\"\n\n[requires]\nformula_compiler = \">=2.0.0\"\n\n\
    [[steps]]\nid = \"impl\"\ntitle = \"Implement\"\n\n[steps.check]\nmax_attempts = 3\n\n\
    [steps.check.check]\nmode = \"exec\"\npath = \"verify.sh\"\ntimeout = \"1m\"\n";

/// Master plan: "check loop passes on 2nd iteration".
#[test]
fn check_loop_passes_on_the_second_iteration() {
    let dir = tempfile::tempdir().unwrap();
    let (root, rig) = scaffold(dir.path(), 10);
    write_formula(&root, "checked", CHECKED);
    // fails on the first run, passes once the marker exists
    write_script(
        &rig,
        "verify.sh",
        "#!/bin/sh\nif [ -f .checked ]; then exit 0; fi\ntouch .checked\necho first try red >&2\nexit 1\n",
    );
    let _campd = Daemon::spawn(&root, &[]);

    let (run_id, root_bead) = sling_formula(&root, "checked");
    wait_until(&root, "the checked run to finalize", |e| {
        count(e, "run.finalized") == 1
    });
    let events = events_json(&root);

    assert_eq!(count(&events, "check.failed"), 1);
    assert_eq!(count(&events, "check.passed"), 1);
    let failed = events.iter().find(|e| e["type"] == "check.failed").unwrap();
    assert_eq!(failed["data"]["attempt"], 1);
    assert_eq!(failed["data"]["exit_code"], 1);
    let passed = events.iter().find(|e| e["type"] == "check.passed").unwrap();
    assert_eq!(passed["data"]["attempt"], 2);
    // two attempt beads worked the step (anchor + 2 attempts share step_id)
    let attempts = events
        .iter()
        .filter(|e| e["type"] == "bead.created" && e["data"]["step_id"] == "impl")
        .count();
    assert_eq!(attempts, 3, "anchor + exactly two attempts");
    let finalized = finalized_of(&events, &root_bead);
    assert_eq!(finalized["outcome"], "pass");
    assert_eq!(finalized["run_id"], run_id.as_str());

    assert_refold_clean(&root);
}

/// Master plan: "check budget exhaustion fails the run".
#[test]
fn check_budget_exhaustion_fails_the_run() {
    const CHECKED_TWO: &str = "formula = \"checked\"\n\n[requires]\nformula_compiler = \">=2.0.0\"\n\n\
        [[steps]]\nid = \"impl\"\ntitle = \"Implement\"\n\n[steps.check]\nmax_attempts = 2\n\n\
        [steps.check.check]\nmode = \"exec\"\npath = \"verify.sh\"\ntimeout = \"1m\"\n";
    let dir = tempfile::tempdir().unwrap();
    let (root, rig) = scaffold(dir.path(), 10);
    write_formula(&root, "checked", CHECKED_TWO);
    write_script(&rig, "verify.sh", "#!/bin/sh\nexit 1\n");
    let _campd = Daemon::spawn(&root, &[]);

    let (_run_id, root_bead) = sling_formula(&root, "checked");
    wait_until(&root, "the exhausted run to finalize", |e| {
        count(e, "run.finalized") == 1
    });
    let events = events_json(&root);

    assert_eq!(count(&events, "check.failed"), 2, "both iterations spent");
    assert_eq!(count(&events, "check.passed"), 0);
    let anchor_close = events
        .iter()
        .find(|e| {
            e["type"] == "bead.closed"
                && e["data"]["reason"]
                    .as_str()
                    .is_some_and(|r| r.contains("check budget (2) exhausted"))
        })
        .expect("the anchor closes naming the budget");
    assert_eq!(anchor_close["data"]["outcome"], "fail");
    assert_eq!(anchor_close["data"]["final_disposition"], "hard_fail");
    let finalized = finalized_of(&events, &root_bead);
    assert_eq!(finalized["outcome"], "fail");
    assert_eq!(finalized["final_disposition"], "hard_fail");

    assert_refold_clean(&root);
}

/// Master plan: "transient retry exhaustion → hard vs soft table".
/// Rows: hard exhaustion fails the run; soft exhaustion without
/// dependents completes it pass/soft_fail; soft exhaustion WITH a
/// dependent skips the dependent and fails the run soft (decision 6).
#[test]
fn transient_retry_exhaustion_hard_vs_soft_table() {
    let retry = |name: &str, disposition: &str, extra: &str| {
        format!(
            "formula = \"{name}\"\n\n[requires]\nformula_compiler = \">=2.0.0\"\n\n\
             [[steps]]\nid = \"fetch\"\ntitle = \"Fetch\"\n\n[steps.retry]\nmax_attempts = 2\n\
             on_exhausted = \"{disposition}\"\n{extra}"
        )
    };
    let dependent = "\n[[steps]]\nid = \"use\"\ntitle = \"Use\"\nneeds = [\"fetch\"]\n";
    let table = [
        (
            "hard",
            retry("hard", "hard_fail", ""),
            "fail",
            "hard_fail",
            0,
        ),
        (
            "soft",
            retry("soft", "soft_fail", ""),
            "pass",
            "soft_fail",
            0,
        ),
        (
            "soft-dep",
            retry("soft-dep", "soft_fail", dependent),
            "fail",
            "soft_fail",
            1,
        ),
    ];
    for (name, toml, run_outcome, run_disposition, skipped) in table {
        let dir = tempfile::tempdir().unwrap();
        let (root, _rig) = scaffold(dir.path(), 10);
        write_formula(&root, name, &toml);
        let plan = dir.path().join("plan");
        std::fs::write(&plan, "fail-transient\nfail-transient\n").unwrap();
        let _campd = Daemon::spawn(&root, &[("FAKE_AGENT_PLAN", plan.to_str().unwrap())]);

        let (_run_id, root_bead) = sling_formula(&root, name);
        wait_until(&root, "the retry run to finalize", |e| {
            count(e, "run.finalized") == 1
        });
        let events = events_json(&root);

        let exhausted = events
            .iter()
            .find(|e| {
                e["type"] == "bead.closed"
                    && e["data"]["reason"]
                        .as_str()
                        .is_some_and(|r| r.contains("retry budget (2) exhausted"))
            })
            .unwrap_or_else(|| panic!("{name}: no exhaustion close"));
        assert_eq!(
            exhausted["data"]["final_disposition"], run_disposition,
            "{name}: the anchor close carries on_exhausted"
        );
        let finalized = finalized_of(&events, &root_bead);
        assert_eq!(finalized["outcome"], run_outcome, "{name}");
        assert_eq!(finalized["final_disposition"], run_disposition, "{name}");
        assert_eq!(
            finalized["skipped"].as_array().unwrap().len(),
            skipped,
            "{name}"
        );
        assert_eq!(
            finalized["soft_failed"].as_array().unwrap().len(),
            usize::from(run_disposition == "soft_fail"),
            "{name}"
        );
        // exactly two attempts ever: the budget bounds the loop
        let attempts = events
            .iter()
            .filter(|e| e["type"] == "bead.created" && e["data"]["step_id"] == "fetch")
            .count();
        assert_eq!(attempts, 3, "{name}: anchor + exactly two attempts");

        assert_refold_clean(&root);
    }
}

const FAN_PARALLEL: &str = "formula = \"fan\"\n\n[requires]\nformula_compiler = \">=2.0.0\"\n\n\
    [[steps]]\nid = \"enumerate\"\ntitle = \"Enumerate\"\n\n[steps.on_complete]\n\
    for_each = \"output.items\"\nbond = \"child\"\n\n[steps.on_complete.vars]\n\
    name = \"{item.name}\"\nposition = \"{index}\"\n";
const FAN_SEQUENTIAL: &str = "formula = \"fan\"\n\n[requires]\nformula_compiler = \">=2.0.0\"\n\n\
    [[steps]]\nid = \"enumerate\"\ntitle = \"Enumerate\"\n\n[steps.on_complete]\n\
    for_each = \"output.items\"\nbond = \"child\"\nsequential = true\n\n[steps.on_complete.vars]\n\
    name = \"{item.name}\"\nposition = \"{index}\"\n";
const CHILD: &str =
    "formula = \"child\"\n\n[[steps]]\nid = \"work\"\ntitle = \"Handle {name} at {position}\"\n";

fn fan_fixture(dir: &Path, fan: &str) -> (PathBuf, PathBuf, PathBuf) {
    let (root, rig) = scaffold(dir, 10);
    write_formula(&root, "fan", fan);
    write_formula(&root, "child", CHILD);
    let output = dir.join("items.json");
    std::fs::write(
        &output,
        r#"{"items":[{"name":"a"},{"name":"b"},{"name":"c"}]}"#,
    )
    .unwrap();
    (root, rig, output)
}

/// Master plan: "on_complete over a 3-item output fans out 3 bonds
/// (parallel …)". Every worker close carries the output (harmless for
/// children — no on_complete there).
#[test]
fn on_complete_fans_out_three_bonds_in_parallel() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig, output) = fan_fixture(dir.path(), FAN_PARALLEL);
    let _campd = Daemon::spawn(
        &root,
        &[("FAKE_AGENT_OUTPUT_JSON", output.to_str().unwrap())],
    );

    let (_run_id, parent_root) = sling_formula(&root, "fan");
    // the parent + 3 children all finalize
    wait_until(&root, "4 finalized runs", |e| {
        count(e, "run.finalized") == 4
    });
    let events = events_json(&root);

    assert_eq!(count(&events, "run.cooked"), 4, "parent + 3 bonds");
    let finalized = finalized_of(&events, &parent_root);
    assert_eq!(finalized["outcome"], "pass");
    // vars substituted into the child steps
    for title in ["Handle a at 0", "Handle b at 1", "Handle c at 2"] {
        assert!(
            events
                .iter()
                .any(|e| e["type"] == "bead.created" && e["data"]["title"] == title),
            "missing child step {title}"
        );
    }
    // child roots carry the bond linkage labels
    let labels: Vec<String> = events
        .iter()
        .filter(|e| e["type"] == "bead.created")
        .filter_map(|e| e["data"]["labels"][0].as_str().map(str::to_owned))
        .filter(|l| l.starts_with("bond:"))
        .collect();
    assert_eq!(labels.len(), 3, "{labels:?}");

    assert_refold_clean(&root);
}

/// Master plan: "… (and sequential)". Child i+1 cooks only after child i's
/// root closes pass — asserted in ledger order — and chains via needs.
#[test]
fn on_complete_fans_out_three_bonds_sequentially() {
    let dir = tempfile::tempdir().unwrap();
    let (root, _rig, output) = fan_fixture(dir.path(), FAN_SEQUENTIAL);
    let _campd = Daemon::spawn(
        &root,
        &[("FAKE_AGENT_OUTPUT_JSON", output.to_str().unwrap())],
    );

    let (_run_id, _parent_root) = sling_formula(&root, "fan");
    wait_until(&root, "4 finalized runs", |e| {
        count(e, "run.finalized") == 4
    });
    let events = events_json(&root);

    // identify the child runs in cook order
    let cooks: Vec<&serde_json::Value> = events
        .iter()
        .filter(|e| e["type"] == "run.cooked" && e["data"]["formula"] == "child")
        .collect();
    assert_eq!(cooks.len(), 3);
    let child_roots: Vec<String> = cooks
        .iter()
        .map(|c| c["data"]["root"].as_str().unwrap().to_owned())
        .collect();
    // serialization, in the ledger's own order: child i+1's cook comes
    // after child i's root close
    for i in 0..2 {
        let close_i = seq_of(&events, |e| {
            e["type"] == "bead.closed" && e["bead"] == child_roots[i].as_str()
        });
        let cook_next = cooks[i + 1]["seq"].as_i64().unwrap();
        assert!(
            cook_next > close_i,
            "child {} cooked (seq {cook_next}) before child {i}'s root closed (seq {close_i})",
            i + 1
        );
    }
    // the literal chain edge: child i+1's root needs child i's root
    for i in 0..2 {
        let created = events
            .iter()
            .find(|e| e["type"] == "bead.created" && e["bead"] == child_roots[i + 1].as_str())
            .unwrap();
        let needs: Vec<&str> = created["data"]["needs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(
            needs.contains(&child_roots[i].as_str()),
            "child {} needs {needs:?}",
            i + 1
        );
    }

    assert_refold_clean(&root);
}

/// kill -9 campd between an attempt's pass close and the check verdict:
/// the restart's reconciliation re-runs the interrupted check and the run
/// completes — crash-only, self-healing (invariant 3, spec §8.5 spirit).
#[test]
fn kill9_between_attempt_close_and_check_verdict_self_heals() {
    let dir = tempfile::tempdir().unwrap();
    let (root, rig) = scaffold(dir.path(), 10);
    write_formula(&root, "checked", CHECKED);
    // slow enough that the kill lands before the verdict
    write_script(&rig, "verify.sh", "#!/bin/sh\nsleep 2\nexit 0\n");
    let campd = Daemon::spawn(&root, &[]);

    let (_run_id, root_bead) = sling_formula(&root, "checked");
    // wait for the attempt's pass close (the worker is done; the check is
    // now sleeping), then kill -9 before its verdict can land
    wait_until(&root, "the attempt to close", |e| {
        e.iter()
            .any(|ev| ev["type"] == "bead.closed" && ev["data"]["reason"] == "fake agent done")
    });
    campd.kill9();
    let events = events_json(&root);
    assert_eq!(
        count(&events, "check.passed"),
        0,
        "the verdict must not have landed yet (kill was too slow otherwise)"
    );

    // restart: reconcile re-queues the interrupted check; the run completes
    let _campd2 = Daemon::spawn(&root, &[]);
    wait_until(&root, "the healed run to finalize", |e| {
        count(e, "run.finalized") == 1
    });
    let events = events_json(&root);
    assert_eq!(count(&events, "check.passed"), 1, "exactly one verdict");
    let finalized = finalized_of(&events, &root_bead);
    assert_eq!(finalized["outcome"], "pass");

    assert_refold_clean(&root);
}

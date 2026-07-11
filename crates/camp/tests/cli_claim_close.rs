#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use assert_cmd::Command;

fn camp() -> Command {
    let mut cmd = Command::cargo_bin("camp").unwrap();
    cmd.env_remove("CAMP_DIR");
    cmd
}

fn camp_with_bead() -> tempfile::TempDir {
    camp_with_bead_in(|_| ())
}

/// Run git in `repo` with hermetic identity/signing (a global
/// commit.gpgsign=true must not stall tests — spawn.rs::git_rig precedent).
fn git(repo: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// camp init + one rig + one bead (gc-1), with the rig prepared by
/// `prepare` (git init / commits) BEFORE any session registers against it.
fn camp_with_bead_in(prepare: impl Fn(&std::path::Path)) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    camp()
        .current_dir(dir.path())
        .args(["init", "--no-service"])
        .assert()
        .success();
    let rig_dir = dir.path().join("repo");
    std::fs::create_dir_all(&rig_dir).unwrap();
    prepare(&rig_dir);
    camp()
        .current_dir(dir.path())
        .args(["rig", "add"])
        .arg(&rig_dir)
        .args(["--prefix", "gc", "--name", "gascity"])
        .assert()
        .success();
    camp()
        .current_dir(dir.path())
        .args(["create", "do the thing", "--rig", "gascity"])
        .assert()
        .success();
    dir
}

fn based_rig(repo: &std::path::Path) {
    git(repo, &["init", "-b", "main"]);
    git(repo, &["commit", "--allow-empty", "-m", "init"]);
}

fn baseless_rig(repo: &std::path::Path) {
    git(repo, &["init", "-b", "main"]); // unborn HEAD: no commit
}

/// Register + claim gc-1 for `camp/dev/1` against rig `gascity` — the
/// woke's `base` is whatever the rig had at this moment.
fn register_and_claim(dir: &std::path::Path) {
    camp()
        .current_dir(dir)
        .args([
            "session",
            "register",
            "--name",
            "camp/dev/1",
            "--agent",
            "dev",
            "--rig",
            "gascity",
            "--session-id",
            "7bd2befc-b018-4080-8738-429d541b3646",
        ])
        .assert()
        .success();
    camp()
        .current_dir(dir)
        .args(["claim", "gc-1", "--session", "camp/dev/1"])
        .assert()
        .success();
}

#[test]
fn claim_then_close_runs_the_full_lifecycle() {
    let dir = camp_with_bead();
    camp()
        .current_dir(dir.path())
        .args(["claim", "gc-1", "--session", "camp/dev/1"])
        .assert()
        .success();
    camp()
        .current_dir(dir.path())
        .args(["close", "gc-1", "--outcome", "pass", "--reason", "shipped"])
        .assert()
        .success();
    // ledger stays refold-clean across the whole lifecycle
    camp()
        .current_dir(dir.path())
        .args(["doctor", "--refold"])
        .assert()
        .success()
        .stdout(predicates::str::contains("0 drift rows"));
}

#[test]
fn double_claim_fails_fast() {
    let dir = camp_with_bead();
    camp()
        .current_dir(dir.path())
        .args(["claim", "gc-1", "--session", "camp/dev/1"])
        .assert()
        .success();
    camp()
        .current_dir(dir.path())
        .args(["claim", "gc-1", "--session", "camp/dev/2"])
        .assert()
        .failure()
        .code(1);
}

#[test]
fn close_rejects_a_non_subset_outcome() {
    let dir = camp_with_bead();
    // clap constrains --outcome to pass|fail (usage error, exit 2)
    camp()
        .current_dir(dir.path())
        .args(["close", "gc-1", "--outcome", "skipped"])
        .assert()
        .failure()
        .code(2);
}

// ---- Phase 9 Task 4: close classification and structured output ----------

fn close_event_data(dir: &tempfile::TempDir) -> serde_json::Value {
    let out = camp()
        .current_dir(dir.path())
        .args(["events", "--json"])
        .output()
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    stdout
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .find(|e| e["type"] == "bead.closed")
        .expect("a bead.closed event")["data"]
        .clone()
}

#[test]
fn transient_requires_a_fail_outcome() {
    let dir = camp_with_bead();
    let assert = camp()
        .current_dir(dir.path())
        .args(["close", "gc-1", "--outcome", "pass", "--transient"])
        .assert()
        .failure();
    let err = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(
        err.contains("--transient") && err.contains("fail"),
        "error must name the rule: {err}"
    );
}

#[test]
fn a_transient_fail_close_carries_the_failure_class() {
    let dir = camp_with_bead();
    camp()
        .current_dir(dir.path())
        .args(["close", "gc-1", "--outcome", "fail", "--transient"])
        .assert()
        .success();
    let data = close_event_data(&dir);
    assert_eq!(data["failure_class"], "transient");
    assert_eq!(data["outcome"], "fail");
}

#[test]
fn output_json_embeds_the_file_and_stdin() {
    let dir = camp_with_bead();
    let path = dir.path().join("out.json");
    std::fs::write(&path, r#"{"items":[{"name":"a"},{"name":"b"}]}"#).unwrap();
    camp()
        .current_dir(dir.path())
        .args(["close", "gc-1", "--outcome", "pass", "--output-json"])
        .arg(&path)
        .assert()
        .success();
    let data = close_event_data(&dir);
    assert_eq!(data["output"]["items"][1]["name"], "b");

    // "-" reads stdin
    let dir = camp_with_bead();
    camp()
        .current_dir(dir.path())
        .args(["close", "gc-1", "--outcome", "pass", "--output-json", "-"])
        .write_stdin(r#"{"n": 3}"#)
        .assert()
        .success();
    let data = close_event_data(&dir);
    assert_eq!(data["output"]["n"], 3);
}

// ---- Phase 3 (#34): the WorkOutcome axis at the CLI --------------------

/// Phase 3 (#34): the WorkOutcome axis at the CLI. no-op/blocked/abandoned
/// need no git facts — accepted here; shipped is gated (Task 7 tests).
#[test]
fn close_records_a_no_op_work_outcome() {
    let dir = camp_with_bead();
    camp()
        .current_dir(dir.path())
        .args([
            "close",
            "gc-1",
            "--outcome",
            "pass",
            "--work-outcome",
            "no-op",
            "--reason",
            "already satisfied",
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains("closed gc-1 (pass, no-op)"));
}

#[test]
fn close_rejects_incoherent_axis_pairings_at_the_prompt() {
    let dir = camp_with_bead();
    // the #34 lie: pass over blocked work — rejected before any append
    camp()
        .current_dir(dir.path())
        .args([
            "close",
            "gc-1",
            "--outcome",
            "pass",
            "--work-outcome",
            "blocked",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("requires --outcome fail"));
    // artifact flags without the axis
    camp()
        .current_dir(dir.path())
        .args([
            "close",
            "gc-1",
            "--outcome",
            "pass",
            "--work-commit",
            "deadbeef",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("--work-outcome shipped"));
    // clap vocabulary: an unknown work outcome is a usage error
    camp()
        .current_dir(dir.path())
        .args([
            "close",
            "gc-1",
            "--outcome",
            "pass",
            "--work-outcome",
            "delivered",
        ])
        .assert()
        .failure()
        .code(2);
}

/// Obligation (iv): a close WITHOUT the new flags appends a payload with
/// exactly the v1 keys — the control axis and its event shape are
/// unchanged, byte for byte.
#[test]
fn a_plain_close_payload_is_byte_identical_to_v1() {
    let dir = camp_with_bead();
    camp()
        .current_dir(dir.path())
        .args(["close", "gc-1", "--outcome", "pass", "--reason", "done"])
        .assert()
        .success();
    let data = close_event_data(&dir);
    let mut keys: Vec<&str> = data
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, ["outcome", "reason"]);
}

/// Decision 10 lockstep: every close flag the worker skill advertises is a
/// real flag — the contract text and the CLI cannot drift.
#[test]
fn close_help_documents_every_flag_the_worker_skill_advertises() {
    let out = camp().args(["close", "--help"]).output().unwrap();
    let help = String::from_utf8_lossy(&out.stdout).to_string();
    let skill = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../plugin/skills/worker/SKILL.md"),
    )
    .unwrap();
    for flag in [
        "--work-outcome",
        "--work-commit",
        "--work-branch",
        "--transient",
        "--output-json",
    ] {
        assert!(skill.contains(flag), "worker skill should advertise {flag}");
        assert!(
            help.contains(flag),
            "camp close --help must document {flag}"
        );
    }
}

// ---- Task 7: the shipped gate — mechanical git facts ---------------------

/// Obligation (i), CLI half (#34's exact scenario): a dead-end root commit
/// on a baseless rig can NEVER close shipped — there was no dispatch-time
/// base, so nothing can have landed. The honest close is fail+blocked, and
/// that is what the ledger records.
#[test]
fn shipped_is_rejected_without_a_dispatch_base_and_blocked_records() {
    let dir = camp_with_bead_in(baseless_rig);
    register_and_claim(dir.path());
    let rig = dir.path().join("repo");
    // the stray dead-end commit (what #34's worker did)
    git(&rig, &["checkout", "-b", "add-readme"]);
    std::fs::write(rig.join("README.md"), "readme\n").unwrap();
    git(&rig, &["add", "README.md"]);
    git(&rig, &["commit", "-m", "dead-end readme"]);
    let sha = git(&rig, &["rev-parse", "HEAD"]);

    camp()
        .current_dir(dir.path())
        .args([
            "close",
            "gc-1",
            "--outcome",
            "pass",
            "--work-outcome",
            "shipped",
            "--work-commit",
            &sha,
            "--work-branch",
            "add-readme",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("no dispatch-time base"));
    // the rejected close appended NOTHING
    let out = camp()
        .current_dir(dir.path())
        .args(["events", "--json"])
        .output()
        .unwrap();
    assert!(!String::from_utf8_lossy(&out.stdout).contains("bead.closed"));

    camp()
        .current_dir(dir.path())
        .args([
            "close",
            "gc-1",
            "--outcome",
            "fail",
            "--work-outcome",
            "blocked",
            "--reason",
            "no base; the branch cannot land",
        ])
        .assert()
        .success();
    let events = String::from_utf8_lossy(
        &camp()
            .current_dir(dir.path())
            .args(["events", "--json"])
            .output()
            .unwrap()
            .stdout,
    )
    .into_owned();
    assert!(events.contains(r#""work_outcome":"blocked""#), "{events}");
    assert!(
        !events.contains(r#""work_outcome":"shipped""#),
        "never shipped: {events}"
    );
}

/// Obligation (ii), CLI half: on a based rig, a commit that descends from
/// the dispatch-time base and is reachable on its branch closes shipped.
#[test]
fn shipped_verifies_reachable_and_based_then_records() {
    let dir = camp_with_bead_in(based_rig);
    register_and_claim(dir.path());
    let rig = dir.path().join("repo");
    git(&rig, &["checkout", "-b", "camp/gc-1"]);
    std::fs::write(rig.join("work.txt"), "the change\n").unwrap();
    git(&rig, &["add", "work.txt"]);
    git(&rig, &["commit", "-m", "the work"]);
    let sha = git(&rig, &["rev-parse", "HEAD"]);

    camp()
        .current_dir(dir.path())
        .args([
            "close",
            "gc-1",
            "--outcome",
            "pass",
            "--reason",
            "done",
            "--work-outcome",
            "shipped",
            "--work-commit",
            &sha,
            "--work-branch",
            "camp/gc-1",
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains("closed gc-1 (pass, shipped)"));
    let events = String::from_utf8_lossy(
        &camp()
            .current_dir(dir.path())
            .args(["events", "--json"])
            .output()
            .unwrap()
            .stdout,
    )
    .into_owned();
    assert!(events.contains(r#""work_outcome":"shipped""#), "{events}");
    assert!(events.contains(&sha), "{events}");
}

/// The gate is fact-checking, not vibes: a wrong branch, an unbased orphan
/// commit, a flag-shaped value, and an unclaimed bead each fail with a
/// message naming the failed fact — and nothing is appended.
#[test]
fn shipped_rejects_unreachable_unbased_flag_shaped_and_unclaimed_facts() {
    let dir = camp_with_bead_in(based_rig);
    register_and_claim(dir.path());
    let rig = dir.path().join("repo");
    let head = git(&rig, &["rev-parse", "HEAD"]);
    // `--flag=value` form: clap's default hyphen handling would intercept
    // a bare `-x` VALUE at the parser (usage error), but the `=` form
    // passes it straight through — the exact path gc's injection guard
    // exists for, and camp's guard must catch it, not clap.
    let close_shipped = |commit: &str, branch: &str| {
        camp()
            .current_dir(dir.path())
            .args([
                "close",
                "gc-1",
                "--outcome",
                "pass",
                "--work-outcome",
                "shipped",
                &format!("--work-commit={commit}"),
                &format!("--work-branch={branch}"),
            ])
            .output()
            .unwrap()
    };
    // a branch name that is not a real local ref (PR #54 review finding 1:
    // this must fail as a missing REF, before any ancestry test could be
    // fooled by a commit-ish)
    let out = close_shipped(&head, "no-such-branch");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("is not a local branch"));
    // unbased: an orphan branch on a based rig does not descend from the base
    git(&rig, &["checkout", "--orphan", "lone"]);
    git(&rig, &["commit", "--allow-empty", "-m", "orphan"]);
    let orphan = git(&rig, &["rev-parse", "HEAD"]);
    git(&rig, &["checkout", "main"]);
    let out = close_shipped(&orphan, "lone");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("does not descend from"));
    // unreachable: a real branch that does not contain the commit
    let out = close_shipped(&head, "lone");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("not reachable on"));
    // flag-shaped values are rejected outright (gc's injection guard)
    let out = close_shipped("-x", "main");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("must not begin with '-'"));
    // an unclaimed bead has no session, hence no base to verify against
    camp()
        .current_dir(dir.path())
        .args(["create", "second", "--rig", "gascity"])
        .assert()
        .success();
    let out = camp()
        .current_dir(dir.path())
        .args([
            "close",
            "gc-2",
            "--outcome",
            "pass",
            "--work-outcome",
            "shipped",
            "--work-commit",
            &head,
            "--work-branch",
            "main",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no claiming session"));
}

#[test]
fn malformed_output_json_fails_fast_naming_the_source() {
    let dir = camp_with_bead();
    let path = dir.path().join("bad.json");
    std::fs::write(&path, "not json").unwrap();
    let assert = camp()
        .current_dir(dir.path())
        .args(["close", "gc-1", "--outcome", "pass", "--output-json"])
        .arg(&path)
        .assert()
        .failure();
    let err = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(err.contains("bad.json"), "error must name the file: {err}");
    // nothing landed: the bead is still open
    camp()
        .current_dir(dir.path())
        .args(["close", "gc-1", "--outcome", "pass"])
        .assert()
        .success();
}

/// PR #54 review finding 1 (HIGH): the gate must reject SELF-CERTIFICATION
/// on a based rig — shipped requires at least one commit of NEW work that
/// DESCENDS from the dispatch-time base, on a REAL local branch. A worker
/// that committed nothing (work_commit == the base, or an ancestor of it)
/// and a SHA passed as the "branch" (a commit is its own ancestor) must
/// each fail — otherwise the ledger records shipped for zero delivered
/// work, and a bare-SHA "branch" is git-gc-able (nothing outlives the
/// reap).
#[test]
fn shipped_rejects_the_base_itself_ancestors_of_base_and_sha_as_branch() {
    let dir = camp_with_bead_in(|repo| {
        based_rig(repo);
        git(repo, &["commit", "--allow-empty", "-m", "second"]);
    });
    register_and_claim(dir.path()); // base = HEAD = the "second" commit
    let rig = dir.path().join("repo");
    let base = git(&rig, &["rev-parse", "HEAD"]);
    let parent = git(&rig, &["rev-parse", "HEAD^"]);
    let close_shipped = |commit: &str, branch: &str| {
        camp()
            .current_dir(dir.path())
            .args([
                "close",
                "gc-1",
                "--outcome",
                "pass",
                "--work-outcome",
                "shipped",
                &format!("--work-commit={commit}"),
                &format!("--work-branch={branch}"),
            ])
            .output()
            .unwrap()
    };

    // (a) the base itself: `git rev-parse HEAD` on an unchanged tree — the
    // exact #34 self-certification, now on a BASED rig. No new work.
    let out = close_shipped(&base, "main");
    assert!(!out.status.success(), "the base itself must not ship");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("at least one commit of new work"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // (a') an abbreviated base sha must not slip past the equality check.
    let out = close_shipped(&base[..12], "main");
    assert!(!out.status.success(), "abbreviated base must not ship");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("at least one commit of new work"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // (b) an ancestor of the base: older than the dispatch point.
    let out = close_shipped(&parent, "main");
    assert!(
        !out.status.success(),
        "an ancestor of the base must not ship"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("does not descend from"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // (c) a SHA as the "branch": a commit is its own ancestor, so without
    // the real-ref requirement the reachability check self-certifies.
    git(&rig, &["checkout", "-b", "camp/gc-1"]);
    git(&rig, &["commit", "--allow-empty", "-m", "real work"]);
    let work = git(&rig, &["rev-parse", "HEAD"]);
    git(&rig, &["checkout", "main"]);
    let out = close_shipped(&work, &work);
    assert!(!out.status.success(), "a SHA is not a branch");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("is not a local branch"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Control: the same real commit on its real branch ships.
    let out = close_shipped(&work, "camp/gc-1");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

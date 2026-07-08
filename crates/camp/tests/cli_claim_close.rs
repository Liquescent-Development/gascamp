#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use assert_cmd::Command;

fn camp() -> Command {
    let mut cmd = Command::cargo_bin("camp").unwrap();
    cmd.env_remove("CAMP_DIR");
    cmd
}

fn camp_with_bead() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    camp()
        .current_dir(dir.path())
        .arg("init")
        .assert()
        .success();
    let rig_dir = dir.path().join("repo");
    std::fs::create_dir_all(&rig_dir).unwrap();
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

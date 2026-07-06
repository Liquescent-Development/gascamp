#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::process::Command as StdCommand;
use std::thread;

use assert_cmd::Command;

fn camp() -> Command {
    let mut cmd = Command::cargo_bin("camp").unwrap();
    cmd.env_remove("CAMP_DIR");
    cmd
}

/// Spawn `n` `camp rig add` processes simultaneously in `camp_dir`; the args
/// after `rig add` for process `i` come from `args_for(i)`. Returns how many
/// exited 0. Used to exercise the advisory lock that serializes rig add
/// (decision H).
fn concurrent_rig_adds(
    camp_dir: &Path,
    n: usize,
    args_for: impl Fn(usize) -> Vec<String>,
) -> usize {
    let bin = assert_cmd::cargo::cargo_bin("camp");
    let all_args: Vec<Vec<String>> = (0..n).map(&args_for).collect();
    thread::scope(|scope| {
        let handles: Vec<_> = all_args
            .into_iter()
            .map(|args| {
                let bin = &bin;
                scope.spawn(move || {
                    StdCommand::new(bin)
                        .current_dir(camp_dir)
                        .env_remove("CAMP_DIR")
                        .arg("rig")
                        .arg("add")
                        .args(&args)
                        .status()
                        .unwrap()
                        .success()
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .filter(|&ok| ok)
            .count()
    })
}

fn count_rig_blocks(camp_root: &Path) -> usize {
    std::fs::read_to_string(camp_root.join(".camp/camp.toml"))
        .unwrap()
        .matches("[[rigs]]")
        .count()
}

/// A camp plus a throwaway directory to register as a rig.
fn camp_with_rig_dir() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    camp()
        .current_dir(dir.path())
        .arg("init")
        .assert()
        .success();
    let rig_dir = dir.path().join("myrepo");
    std::fs::create_dir_all(&rig_dir).unwrap();
    (dir, rig_dir)
}

#[test]
fn rig_add_writes_toml_and_appends_event() {
    let (dir, rig_dir) = camp_with_rig_dir();
    camp()
        .current_dir(dir.path())
        .args(["rig", "add"])
        .arg(&rig_dir)
        .args(["--prefix", "gc", "--name", "gascity"])
        .assert()
        .success();

    let toml = std::fs::read_to_string(dir.path().join(".camp/camp.toml")).unwrap();
    assert!(toml.contains("[[rigs]]"), "toml was: {toml}");
    assert!(toml.contains("name = \"gascity\""));
    assert!(toml.contains("prefix = \"gc\""));

    let events = camp()
        .current_dir(dir.path())
        .args(["events", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let events = String::from_utf8(events).unwrap();
    assert!(events.contains(r#""type":"rig.added""#), "events: {events}");

    // rig ls shows it
    camp()
        .current_dir(dir.path())
        .args(["rig", "ls"])
        .assert()
        .success()
        .stdout(predicates::str::contains("gascity"));
}

#[test]
fn duplicate_prefix_is_rejected() {
    let (dir, rig_dir) = camp_with_rig_dir();
    let rig_dir2 = dir.path().join("other");
    std::fs::create_dir_all(&rig_dir2).unwrap();
    camp()
        .current_dir(dir.path())
        .args(["rig", "add"])
        .arg(&rig_dir)
        .args(["--prefix", "gc", "--name", "a"])
        .assert()
        .success();
    camp()
        .current_dir(dir.path())
        .args(["rig", "add"])
        .arg(&rig_dir2)
        .args(["--prefix", "gc", "--name", "b"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicates::str::contains("prefix"));
}

#[test]
fn bad_prefix_is_rejected() {
    let (dir, rig_dir) = camp_with_rig_dir();
    camp()
        .current_dir(dir.path())
        .args(["rig", "add"])
        .arg(&rig_dir)
        .args(["--prefix", "Bad-One", "--name", "x"])
        .assert()
        .failure()
        .code(1);
}

/// Two distinct rigs both persist through the locked read-modify-write path
/// (decision H) — the second add appends without clobbering the first.
#[test]
fn two_distinct_rigs_both_persist() {
    let (dir, rig_dir) = camp_with_rig_dir();
    let rig_dir2 = dir.path().join("second");
    std::fs::create_dir_all(&rig_dir2).unwrap();
    camp()
        .current_dir(dir.path())
        .args(["rig", "add"])
        .arg(&rig_dir)
        .args(["--prefix", "gc", "--name", "gascity"])
        .assert()
        .success();
    camp()
        .current_dir(dir.path())
        .args(["rig", "add"])
        .arg(&rig_dir2)
        .args(["--prefix", "t3", "--name", "tools"])
        .assert()
        .success();

    let toml = std::fs::read_to_string(dir.path().join(".camp/camp.toml")).unwrap();
    for needle in ["gascity", "tools", "gc", "t3"] {
        assert!(
            toml.contains(needle),
            "camp.toml missing {needle:?}: {toml}"
        );
    }
    camp()
        .current_dir(dir.path())
        .args(["rig", "ls"])
        .assert()
        .success()
        .stdout(predicates::str::contains("gascity"))
        .stdout(predicates::str::contains("tools"));
}

/// Concurrency regression for decision H: N processes racing to add the SAME
/// rig. The advisory lock serializes the check-append-write section, so
/// exactly one wins (the rest re-read its rig and fail the duplicate check)
/// and camp.toml holds exactly one [[rigs]] block. Without the lock (deleted,
/// or acquired after CampConfig::load) the racers pass a stale duplicate check
/// and several succeed — which this test rejects.
#[test]
fn concurrent_same_rig_add_serializes_to_exactly_one() {
    let (dir, rig_dir) = camp_with_rig_dir();
    let rig_path = rig_dir.to_str().unwrap().to_owned();

    let successes = concurrent_rig_adds(dir.path(), 8, |_| {
        vec![
            rig_path.clone(),
            "--prefix".into(),
            "gc".into(),
            "--name".into(),
            "gascity".into(),
        ]
    });

    assert_eq!(successes, 1, "exactly one concurrent add must win");
    assert_eq!(
        count_rig_blocks(dir.path()),
        1,
        "camp.toml must hold exactly one rig block (no duplicate, no clobber)"
    );
}

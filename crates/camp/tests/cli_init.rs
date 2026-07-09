#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

use assert_cmd::Command;

fn camp() -> Command {
    let mut cmd = Command::cargo_bin("camp").unwrap();
    cmd.env_remove("CAMP_DIR");
    cmd
}

/// `git init` a directory so that init/rig-add see an enclosing repo.
fn git_init(dir: &Path) {
    let ok = std::process::Command::new("git")
        .current_dir(dir)
        .args(["init", "-q"])
        .status()
        .unwrap()
        .success();
    assert!(ok, "git init failed in {}", dir.display());
}

/// True iff `git check-ignore` reports `rel` (relative to `repo`) ignored.
/// `core.excludesFile=/dev/null` neutralizes the developer's global excludes
/// so the result reflects only the repo's `.gitignore` (what init writes).
fn is_ignored(repo: &Path, rel: &str) -> bool {
    std::process::Command::new("git")
        .current_dir(repo)
        .args(["-c", "core.excludesFile=/dev/null", "check-ignore", rel])
        .status()
        .unwrap()
        .success()
}

#[test]
fn init_creates_dot_camp_in_cwd() {
    let dir = tempfile::tempdir().unwrap();
    camp()
        .current_dir(dir.path())
        .arg("init")
        .assert()
        .success()
        .stdout(predicates::str::contains(".camp"));

    assert!(dir.path().join(".camp/camp.toml").exists());
    assert!(dir.path().join(".camp/camp.db").exists());
    let config = std::fs::read_to_string(dir.path().join(".camp/camp.toml")).unwrap();
    assert!(config.contains("[camp]"), "camp.toml was: {config}");
    assert!(config.contains("name = "), "camp.toml was: {config}");
}

#[test]
fn init_with_explicit_camp_dir() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("camps").join("dev");
    camp()
        .current_dir(dir.path())
        .arg("--camp")
        .arg(&target)
        .arg("init")
        .assert()
        .success();

    assert!(target.join("camp.toml").exists());
    assert!(target.join("camp.db").exists());
}

/// Issue #35: init inside a git repo must gitignore the camp's live runtime
/// state (the SQLite ledger, WAL/SHM sidecars, daemon socket + bind lock, and
/// log) so `git add .` never stages a live database or socket — while leaving
/// `camp.toml`, the human-authored source of truth (spec §7.1/§13.4), tracked.
#[test]
fn init_gitignores_live_runtime_but_not_camp_toml() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git_init(repo);

    camp().current_dir(repo).arg("init").assert().success();

    for runtime in [
        ".camp/camp.db",
        ".camp/camp.db-wal",
        ".camp/camp.db-shm",
        ".camp/campd.sock",
        ".camp/campd.sock.lock",
        ".camp/campd.log",
    ] {
        assert!(
            is_ignored(repo, runtime),
            "{runtime} must be gitignored after init"
        );
    }

    // The source-of-truth config stays tracked.
    assert!(
        !is_ignored(repo, ".camp/camp.toml"),
        ".camp/camp.toml must NOT be gitignored (source of truth, decision D)"
    );
}

/// A camp nested below the repo root gets anchored gitignore rules relative to
/// the repo root, so `git check-ignore` resolves the nested runtime state.
#[test]
fn init_gitignores_nested_camp_with_anchored_paths() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git_init(repo);
    let camp_dir = repo.join("sub").join(".camp");

    camp()
        .current_dir(repo)
        .arg("--camp")
        .arg(&camp_dir)
        .arg("init")
        .assert()
        .success();

    assert!(
        is_ignored(repo, "sub/.camp/camp.db"),
        "nested live db must be gitignored"
    );
    assert!(
        !is_ignored(repo, "sub/.camp/camp.toml"),
        "nested camp.toml must NOT be gitignored"
    );
    let gitignore = std::fs::read_to_string(repo.join(".gitignore")).unwrap();
    assert!(
        gitignore.contains("/sub/.camp/camp.db\n"),
        "anchored to repo root: {gitignore}"
    );
}

/// Outside a git repo, init must not fabricate a `.gitignore`.
#[test]
fn init_outside_git_repo_creates_no_gitignore() {
    let dir = tempfile::tempdir().unwrap();
    camp()
        .current_dir(dir.path())
        .arg("init")
        .assert()
        .success();
    assert!(
        !dir.path().join(".gitignore").exists(),
        "no .gitignore may be created outside a git repo"
    );
    assert!(
        !dir.path().join(".camp/.gitignore").exists(),
        "no .gitignore may be created inside .camp either"
    );
}

/// Re-running the gitignore-ensuring path (init then rig add, both entry
/// points) must not duplicate an entry: idempotent by construction.
#[test]
fn init_then_rig_add_gitignore_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git_init(repo);
    camp().current_dir(repo).arg("init").assert().success();

    let after_init = std::fs::read_to_string(repo.join(".gitignore")).unwrap();
    assert_eq!(
        after_init.matches("/.camp/camp.db\n").count(),
        1,
        "exactly one camp.db entry after init: {after_init}"
    );

    let rig_dir = repo.join("myrepo");
    std::fs::create_dir_all(&rig_dir).unwrap();
    camp()
        .current_dir(repo)
        .args(["rig", "add"])
        .arg(&rig_dir)
        .args(["--prefix", "gc", "--name", "gascity"])
        .assert()
        .success();

    let after_rig = std::fs::read_to_string(repo.join(".gitignore")).unwrap();
    assert_eq!(
        after_rig.matches("/.camp/camp.db\n").count(),
        1,
        "rig add must not duplicate the camp.db entry: {after_rig}"
    );
    assert!(is_ignored(repo, ".camp/camp.db"));
}

#[test]
fn reinit_fails_fast() {
    let dir = tempfile::tempdir().unwrap();
    camp()
        .current_dir(dir.path())
        .arg("init")
        .assert()
        .success();
    camp()
        .current_dir(dir.path())
        .arg("init")
        .assert()
        .failure()
        .code(1)
        .stderr(predicates::str::contains("already"));
}

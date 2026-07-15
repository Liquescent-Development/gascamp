#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! compat §14-style hermetic gate for compat-4: drives `camp mail`, the
//! `gc-shim mail`/`prime` verbs, and the exit-code contract through the REAL
//! `camp` binary against a fixture camp. No network, no API.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_camp");

fn scaffold(dir: &Path) -> std::path::PathBuf {
    let root = dir.join(".camp");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("camp.toml"),
        "[camp]\nname = \"t\"\n\n[[rigs]]\nname = \"gc\"\npath = \".\"\nprefix = \"gc\"\n",
    )
    .unwrap();
    root
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

#[test]
fn mail_check_exit_code_follows_the_gc_contract() {
    let dir = tempfile::tempdir().unwrap();
    let root = scaffold(dir.path());
    assert_eq!(
        camp(&root, &["mail", "check"]).status.code(),
        Some(1),
        "empty inbox = exit 1 (A2)"
    );
    let sent = camp(&root, &["mail", "send", "human", "-s", "Approve?", "-m", "the spec"]);
    assert!(
        sent.status.success(),
        "{}",
        String::from_utf8_lossy(&sent.stderr)
    );
    assert_eq!(
        camp(&root, &["mail", "check"]).status.code(),
        Some(0),
        "has mail = exit 0 (A2)"
    );
}

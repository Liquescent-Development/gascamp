#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Phase 14: `camp export --city <dir>` against the real binary
//! (spec §15.3). The byte-level golden lives in camp-core; this file pins
//! the CLI surface: exit codes, stderr listings, the skip flag, and the
//! non-empty-dir refusal.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_camp");

fn camp_cmd(root: &Path) -> Command {
    let mut cmd = Command::new(BIN);
    cmd.env_remove("CAMP_DIR").arg("--camp").arg(root);
    cmd
}

fn run_ok(root: &Path, args: &[&str]) -> String {
    let out = camp_cmd(root).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "camp {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// camp init + one rig + agents/dev.md + seeded beads:
/// gc-1 closed-with-outcome, gc-2 open needing gc-1, gc-3 mail, gc-4 memory.
fn init_camp(dir: &Path) -> PathBuf {
    let status = Command::new(BIN)
        .env_remove("CAMP_DIR")
        .current_dir(dir)
        .arg("init")
        .status()
        .unwrap();
    assert!(status.success());
    let root = dir.join(".camp");
    let rig = dir.join("repo");
    std::fs::create_dir_all(&rig).unwrap();
    let out = camp_cmd(&root)
        .args(["rig", "add"])
        .arg(&rig)
        .args(["--prefix", "gc", "--name", "gc"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    std::fs::create_dir_all(root.join("formulas")).unwrap();
    std::fs::write(
        root.join("formulas/one-step.toml"),
        "formula = \"one-step\"\n\n[[steps]]\nid = \"s1\"\ntitle = \"one step\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("agents")).unwrap();
    std::fs::write(root.join("agents/dev.md"), "# dev agent\n").unwrap();

    run_ok(
        &root,
        &[
            "create",
            "implement widget",
            "--description",
            "the change",
            "--label",
            "cli",
        ],
    );
    run_ok(&root, &["claim", "gc-1", "--session", "camp/dev/1"]);
    run_ok(
        &root,
        &["close", "gc-1", "--outcome", "pass", "--reason", "shipped"],
    );
    run_ok(&root, &["create", "review widget", "--needs", "gc-1"]);
    run_ok(&root, &["create", "ping from ci", "--type", "mail"]);
    run_ok(&root, &["remember", "deploy needs the VPN profile"]);
    root
}

fn add_orders(root: &Path, table: &str) {
    let path = root.join("camp.toml");
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str(table);
    std::fs::write(&path, text).unwrap();
}

const TRANSLATABLE: &str = r#"
[[order]]
name    = "nightly"
on      = "cron:0 7 * * 1-5"
formula = "one-step"

[[order]]
name    = "on-close"
on      = "event:bead.closed"
formula = "one-step"
"#;

const LABELED: &str = r#"
[[order]]
name    = "ci-red"
on      = "event:bead.closed[label=ci-red]"
formula = "one-step"
"#;

#[test]
fn export_writes_the_city_directory_and_reports_counts() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());
    add_orders(&root, TRANSLATABLE);
    let city = dir.path().join("city");
    let stdout = run_ok(&root, &["export", "--city", city.to_str().unwrap()]);
    assert!(
        stdout.contains("3 issues") && stdout.contains("1 memories"),
        "{stdout}"
    );

    // every jsonl line parses; the closed bead field-maps
    let text = std::fs::read_to_string(city.join("beads.jsonl")).unwrap();
    let lines: Vec<serde_json::Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 4);
    let gc1 = lines.iter().find(|l| l["id"] == "gc-1").unwrap();
    assert_eq!(gc1["status"], "closed");
    assert_eq!(gc1["metadata"]["gc.outcome"], "pass");
    assert_eq!(gc1["close_reason"], "shipped");
    let gc2 = lines.iter().find(|l| l["id"] == "gc-2").unwrap();
    assert_eq!(gc2["dependencies"][0]["depends_on_id"], "gc-1");
    assert!(lines.iter().any(|l| l["_type"] == "memory"));

    assert!(city.join("pack/pack.toml").exists());
    assert!(city.join("pack/agents/dev.md").exists());
    assert!(city.join("pack/orders/nightly.toml").exists());
    assert!(city.join("pack/orders/on-close.toml").exists());
    assert!(city.join("pack/formulas/one-step.toml").exists());
}

#[test]
fn untranslatable_orders_fail_the_export_listing_them() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());
    add_orders(&root, TRANSLATABLE);
    add_orders(&root, LABELED);
    let city = dir.path().join("city");
    let out = camp_cmd(&root)
        .args(["export", "--city", city.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("ci-red")
            && stderr.contains("label")
            && stderr.contains("--skip-untranslatable"),
        "{stderr}"
    );
    // fail-before-write
    assert_eq!(std::fs::read_dir(&city).unwrap().count(), 0);
}

#[test]
fn skip_untranslatable_is_the_explicit_opt_out() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());
    add_orders(&root, TRANSLATABLE);
    add_orders(&root, LABELED);
    let city = dir.path().join("city");
    let out = camp_cmd(&root)
        .args([
            "export",
            "--city",
            city.to_str().unwrap(),
            "--skip-untranslatable",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("skipped untranslatable order ci-red"),
        "{stderr}"
    );
    assert!(city.join("pack/orders/nightly.toml").exists());
    assert!(!city.join("pack/orders/ci-red.toml").exists());
}

#[test]
fn a_non_empty_target_directory_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let root = init_camp(dir.path());
    let city = dir.path().join("city");
    std::fs::create_dir_all(&city).unwrap();
    std::fs::write(city.join("keep.txt"), "precious").unwrap();
    let out = camp_cmd(&root)
        .args(["export", "--city", city.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("non-empty"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(city.join("keep.txt")).unwrap(),
        "precious"
    );
}

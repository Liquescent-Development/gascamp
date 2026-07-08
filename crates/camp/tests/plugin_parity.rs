#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Command-markdown ↔ CLI flag parity (Phase 12): the plugin's slash
//! commands are thin wrappers over the `camp` CLI (identical scripting
//! surface, spec §13 guarantee 6). Every subcommand and flag a command
//! wrapper invokes must be a real `camp` flag. We scan ONLY the executable
//! `!` fenced block + the argument-hint line — never free prose in the
//! description, which may mention another verb and mis-target (note a).

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_camp");

fn plugin_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugin")
}

fn help(sub: &str) -> String {
    let out = Command::new(BIN).args([sub, "--help"]).output().unwrap();
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    s
}

/// The executable `!` fenced block plus the argument-hint frontmatter line
/// — the only places a wrapper's real invocation lives.
fn scannable(md: &str) -> String {
    let mut out = String::new();
    let mut in_block = false;
    for line in md.lines() {
        let t = line.trim_start();
        if t.starts_with("```!") {
            in_block = true;
            continue;
        }
        if in_block && t.starts_with("```") {
            in_block = false;
            continue;
        }
        if in_block {
            out.push_str(line);
            out.push('\n');
        }
        if let Some(hint) = t.strip_prefix("argument-hint:") {
            out.push_str(hint);
            out.push('\n');
        }
    }
    out
}

fn wrapped_subcommand(scan: &str) -> String {
    let toks: Vec<&str> = scan.split_whitespace().collect();
    toks.windows(2)
        .find(|w| w[0] == "camp")
        .map(|w| w[1].trim_matches(|c: char| !c.is_alphanumeric()).to_owned())
        .expect("a command wrapper's `!` block must invoke `camp <sub>`")
}

/// Extract `--flag` names, tolerating brackets/pipes/parens in the hint
/// (e.g. `[--agent A]`).
fn flags_in(scan: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in scan.split(|c: char| c.is_whitespace() || "[]|()".contains(c)) {
        if let Some(rest) = raw.strip_prefix("--") {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '-')
                .collect();
            if name.len() >= 2 {
                out.push(format!("--{name}"));
            }
        }
    }
    out
}

#[test]
fn every_command_wrapper_uses_only_real_cli_flags() {
    let commands = plugin_dir().join("commands");
    let mut checked = 0;
    for entry in std::fs::read_dir(&commands).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let md = std::fs::read_to_string(&path).unwrap();
        let scan = scannable(&md);
        let sub = wrapped_subcommand(&scan);
        let h = help(&sub);
        for flag in flags_in(&scan) {
            assert!(
                h.contains(&flag),
                "{}: `{}` is not a real `camp {}` flag",
                path.display(),
                flag,
                sub
            );
        }
        checked += 1;
    }
    assert_eq!(checked, 4, "expected exactly four command wrappers");
}

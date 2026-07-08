//! Worker spawn mechanics (spec §8.4, §12). The Phase 2 fixture facts
//! F1–F7 (docs/design/2026-07-06-assumption-findings.md) are BINDING here:
//! F1 pre-assigned --session-id, F2 --output-format json, F3 transcript
//! path from the WORKER's cwd, F5 stdin at /dev/null, F7 per-agent pinning
//! flags. Everything in this module is mechanical; roles live in packs.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use anyhow::{Context, Result, bail};
use camp_core::pack::AgentDef;

/// The worker-contract instructions (spec §8.4: claim → milestones →
/// close → exit). `{bead}` and `{session}` are substituted per spawn; the
/// richer worker *skill* is Phase 12 pack content — this is the mechanical
/// floor every campd-spawned worker gets.
const WORKER_CONTRACT: &str = "You are a Gas Camp worker session working exactly one bead.\n\
Contract, in order:\n\
1. Claim it: run `camp claim {bead} --session {session}`\n\
2. Read it: `camp show {bead}`\n\
3. Do the work in the current directory.\n\
4. As you hit milestones, record them: `camp event emit \"<one line>\" --bead {bead} --session {session}`\n\
5. Close it: `camp close {bead} --outcome pass --reason \"<one line>\"` (or --outcome fail)\n\
6. Exit. Do not start unrelated work. CAMP_DIR is already set for the camp CLI.\n";

fn task_prompt(bead_id: &str, session_name: &str) -> String {
    WORKER_CONTRACT
        .replace("{bead}", bead_id)
        .replace("{session}", session_name)
}

/// Every non-ASCII-alphanumeric CHARACTER becomes one '-' — Claude Code's
/// project dir scheme (F3), reused for the sessions/ capture file names.
/// Unicode note (PR #14 review finding 6): a multi-byte char maps to a
/// single dash; whether real claude munges unicode cwds per byte instead
/// is unverified (F3 verified ASCII only) and is a Phase 11 input — the
/// path is audit-only here.
pub fn munge(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// F1: campd pre-assigns the claude session id.
pub fn new_session_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Where Claude Code keeps its state: $CLAUDE_CONFIG_DIR override, else
/// $HOME/.claude (F3). No HOME is a per-dispatch error, not a campd crash.
pub fn claude_config_root() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let home = std::env::var_os("HOME")
        .context("HOME is not set; cannot compute the worker transcript path (F3)")?;
    Ok(PathBuf::from(home).join(".claude"))
}

/// F3: `<root>/projects/<munge(cwd)>/<sid>.jsonl`, computed from the
/// WORKER's cwd — the worktree path when isolation is on.
pub fn transcript_path_under(root: &Path, worker_cwd: &Path, session_id: &str) -> PathBuf {
    root.join("projects")
        .join(munge(&worker_cwd.to_string_lossy()))
        .join(format!("{session_id}.jsonl"))
}

pub struct SpawnSpec {
    pub session_name: String,
    pub claude_session_id: String,
    pub transcript_path: PathBuf,
    pub cwd: PathBuf,
    pub argv: Vec<OsString>,
    pub env: Vec<(String, String)>,
    pub stdout_path: PathBuf,
    pub stderr_path: PathBuf,
}

/// Assemble the exec plan. Pure — no filesystem, no process. The argv is
/// asserted verbatim by tests against F1/F2/F7 and plan decision L.
#[allow(clippy::too_many_arguments)]
pub fn build_spec(
    command: &Path,
    agent: &AgentDef,
    camp_root: &Path,
    bead_id: &str,
    session_name: &str,
    session_id: &str,
    transcript_path: &Path,
    cwd: &Path,
) -> SpawnSpec {
    let mut argv: Vec<OsString> = vec![command.as_os_str().to_owned()];
    {
        let mut arg = |s: &str| argv.push(OsString::from(s));
        arg("--output-format");
        arg("json"); // F2
        arg("--session-id");
        arg(session_id); // F1
        if let Some(model) = &agent.model {
            arg("--model");
            arg(model); // F7
        }
        if let Some(mode) = &agent.permission_mode {
            arg("--permission-mode");
            arg(mode); // F7
        }
        if let Some(tools) = &agent.tools {
            arg("--allowedTools");
            arg(&tools.join(",")); // F7 (comma-joined list form)
        }
        if !agent.prompt.is_empty() {
            arg("--append-system-prompt");
            arg(&agent.prompt); // decision L: the role prompt
        }
        arg("-p");
        arg(&task_prompt(bead_id, session_name)); // the task
    }

    let sessions_dir = camp_root.join("sessions");
    let file_stem = munge(session_name);
    SpawnSpec {
        session_name: session_name.to_owned(),
        claude_session_id: session_id.to_owned(),
        transcript_path: transcript_path.to_owned(),
        cwd: cwd.to_owned(),
        argv,
        env: vec![
            (
                "CAMP_DIR".to_owned(),
                camp_root.to_string_lossy().into_owned(),
            ),
            ("CAMP_BEAD".to_owned(), bead_id.to_owned()),
            ("CAMP_SESSION".to_owned(), session_name.to_owned()),
        ],
        stdout_path: sessions_dir.join(format!("{file_stem}.json")),
        stderr_path: sessions_dir.join(format!("{file_stem}.log")),
    }
}

/// Exec the worker. stdin is /dev/null (F5 — an open non-pipe stdin costs
/// a 3 s sniff; stream-json stdin-held workers are the Phase 11 nudge
/// path). stdout/stderr go to the sessions/ capture files (decision G).
/// The child is deliberately not waited here: SIGCHLD-driven try_wait in
/// the dispatcher reaps it, and workers intentionally outlive a killed
/// campd (adoption, spec §8.5).
#[allow(clippy::zombie_processes)]
pub fn spawn(spec: &SpawnSpec) -> Result<Child> {
    let sessions_dir = spec
        .stdout_path
        .parent()
        .context("capture path has no parent")?;
    std::fs::create_dir_all(sessions_dir)
        .with_context(|| format!("creating {}", sessions_dir.display()))?;
    let stdout = std::fs::File::create(&spec.stdout_path)
        .with_context(|| format!("creating {}", spec.stdout_path.display()))?;
    let stderr = std::fs::File::create(&spec.stderr_path)
        .with_context(|| format!("creating {}", spec.stderr_path.display()))?;
    let mut cmd = Command::new(&spec.argv[0]);
    cmd.args(&spec.argv[1..])
        .current_dir(&spec.cwd)
        .stdin(Stdio::null()) // F5
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    for (key, value) in &spec.env {
        cmd.env(key, value);
    }
    cmd.spawn()
        .with_context(|| format!("spawning {}", spec.argv[0].to_string_lossy()))
}

/// `git worktree add -b camp/<bead> <camp>/worktrees/<bead>` (decision H).
/// A pre-existing directory or branch fails fast — bead ids are unique and
/// Phase 8 never respawns a bead.
pub fn create_worktree(rig_path: &Path, worktrees_dir: &Path, bead_id: &str) -> Result<PathBuf> {
    std::fs::create_dir_all(worktrees_dir)
        .with_context(|| format!("creating {}", worktrees_dir.display()))?;
    let dir = worktrees_dir.join(bead_id);
    if dir.exists() {
        // The residue hint matters (PR #14 review finding 4): this branch
        // also fires when a session.woke append failed after the worktree
        // was created, and the message must not hide that history.
        bail!(
            "worktree {} already exists (may be residue of an earlier failed dispatch)",
            dir.display()
        );
    }
    let out = Command::new("git")
        .arg("-C")
        .arg(rig_path)
        .args(["worktree", "add", "-b"])
        .arg(format!("camp/{bead_id}"))
        .arg(&dir)
        .output()
        .context("running git worktree add")?;
    if !out.status.success() {
        bail!(
            "git worktree add failed for {}: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(dir)
}

/// Remove a clean worktree (decision H). The camp/<bead> branch is left
/// standing — it may hold unpushed work; sweeping is Phase 11 policy.
pub fn remove_worktree(rig_path: &Path, worktree: &Path) -> Result<()> {
    let out = Command::new("git")
        .arg("-C")
        .arg(rig_path)
        .args(["worktree", "remove", "--force"])
        .arg(worktree)
        .output()
        .context("running git worktree remove")?;
    if !out.status.success() {
        bail!(
            "git worktree remove failed for {}: {}",
            worktree.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use camp_core::pack::{AgentDef, Isolation};
    use std::process::Command;

    fn full_agent() -> AgentDef {
        AgentDef {
            name: "dev".into(),
            model: Some("sonnet".into()),
            tools: Some(vec!["Read".into(), "Edit".into(), "Bash".into()]),
            permission_mode: Some("acceptEdits".into()),
            isolation: Isolation::None,
            prompt: "Implement with TDD.".into(),
        }
    }

    /// Exit criterion pinned as a test: real-claude spawn arguments match
    /// the F1–F7 fixture facts exactly.
    #[test]
    fn argv_matches_the_fixture_facts_for_a_fully_pinned_agent() {
        let sid = "7bd2befc-b018-4080-8738-429d541b3646";
        let spec = build_spec(
            Path::new("claude"),
            &full_agent(),
            Path::new("/camps/dev"),
            "gc-142",
            "dev/dev/1",
            sid,
            Path::new("/home/u/.claude/projects/-code-gc/x.jsonl"),
            Path::new("/code/gc"),
        );
        let argv: Vec<&str> = spec.argv.iter().map(|s| s.to_str().unwrap()).collect();
        // F2: json envelope; F1: pre-assigned session id; F7: per-agent
        // pins; decision L: agent prompt via --append-system-prompt, task
        // via -p.
        assert_eq!(
            argv[..12].to_vec(),
            vec![
                "claude",
                "--output-format",
                "json",
                "--session-id",
                sid,
                "--model",
                "sonnet",
                "--permission-mode",
                "acceptEdits",
                "--allowedTools",
                "Read,Edit,Bash",
                "--append-system-prompt",
            ]
        );
        assert_eq!(argv[12], "Implement with TDD.");
        assert_eq!(argv[13], "-p");
        let task = argv[14];
        assert!(
            task.contains("camp claim gc-142 --session dev/dev/1"),
            "task: {task}"
        );
        assert!(task.contains("camp close gc-142 --outcome"), "task: {task}");
        assert!(task.contains("camp event emit"), "task: {task}");
        assert_eq!(argv.len(), 15);

        assert_eq!(
            spec.env,
            vec![
                ("CAMP_DIR".to_owned(), "/camps/dev".to_owned()),
                ("CAMP_BEAD".to_owned(), "gc-142".to_owned()),
                ("CAMP_SESSION".to_owned(), "dev/dev/1".to_owned()),
            ]
        );
        // decision G: capture paths under <camp>/sessions/
        assert_eq!(
            spec.stdout_path,
            Path::new("/camps/dev/sessions/dev-dev-1.json")
        );
        assert_eq!(
            spec.stderr_path,
            Path::new("/camps/dev/sessions/dev-dev-1.log")
        );
    }

    #[test]
    fn undeclared_agent_fields_emit_no_flags() {
        let agent = AgentDef {
            name: "bare".into(),
            model: None,
            tools: None,
            permission_mode: None,
            isolation: Isolation::None,
            prompt: "P".into(),
        };
        let spec = build_spec(
            Path::new("claude"),
            &agent,
            Path::new("/c"),
            "gc-1",
            "t/bare/1",
            "sid",
            Path::new("/t.jsonl"),
            Path::new("/code"),
        );
        let argv: Vec<&str> = spec.argv.iter().map(|s| s.to_str().unwrap()).collect();
        for flag in ["--model", "--permission-mode", "--allowedTools"] {
            assert!(!argv.contains(&flag), "{flag} must be absent: {argv:?}");
        }
        assert!(argv.contains(&"--append-system-prompt"));
    }

    /// F3, pinned against the Phase 2 D3 probe evidence shape.
    #[test]
    fn transcript_path_munges_every_non_alphanumeric_to_dash() {
        assert_eq!(munge("/tmp/rig-a"), "-tmp-rig-a");
        assert_eq!(munge("/code/gas_camp.rs"), "-code-gas-camp-rs");
        // PR #14 review finding 6: munge is per CHAR — one dash per
        // non-ASCII-alphanumeric character, however many bytes it takes.
        // Whether real claude munges per byte for unicode cwds is a Phase
        // 11 verification input; this pins camp's current behavior.
        assert_eq!(munge("/tmp/héllo"), "-tmp-h-llo");
        assert_eq!(munge("日本"), "--");
        let p = transcript_path_under(
            Path::new("/home/u/.claude"),
            Path::new("/private/tmp/rig-a"),
            "7bd2befc-b018-4080-8738-429d541b3646",
        );
        assert_eq!(
            p,
            Path::new(
                "/home/u/.claude/projects/-private-tmp-rig-a/7bd2befc-b018-4080-8738-429d541b3646.jsonl"
            )
        );
    }

    #[test]
    fn session_ids_are_v4_uuids_and_unique() {
        let a = new_session_id();
        let b = new_session_id();
        assert_ne!(a, b);
        assert_eq!(a.len(), 36);
        assert_eq!(a.as_bytes()[14], b'4', "uuid version nibble must be 4");
    }

    /// Worktree lifecycle against a real git repo (decision H).
    #[test]
    fn worktree_create_and_remove_round_trip() {
        let _spawning = crate::daemon::spawn_probe_guard();
        let dir = tempfile::tempdir().unwrap();
        let rig = dir.path().join("rig");
        std::fs::create_dir_all(&rig).unwrap();
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.email", "t@t"],
            vec!["config", "user.name", "t"],
            // hermetic against operator gitconfig: a global
            // commit.gpgsign=true would stall this fixture on a signer
            // that is not there (observed on the dev machine; CI never
            // signs)
            vec!["config", "commit.gpgsign", "false"],
            vec!["commit", "--allow-empty", "-m", "init"],
        ] {
            let out = Command::new("git")
                .arg("-C")
                .arg(&rig)
                .args(&args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        let worktrees = dir.path().join("worktrees");
        let wt = create_worktree(&rig, &worktrees, "gc-7").unwrap();
        assert_eq!(wt, worktrees.join("gc-7"));
        assert!(wt.join(".git").exists(), "a worktree has a .git link file");
        // fresh branch named for the bead
        let out = Command::new("git")
            .arg("-C")
            .arg(&wt)
            .args(["branch", "--show-current"])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "camp/gc-7");
        // a second create for the same bead fails fast, hinting that the
        // leftover may be residue of an earlier failed dispatch (PR #14
        // review finding 4)
        let err = create_worktree(&rig, &worktrees, "gc-7").unwrap_err();
        assert!(
            err.to_string()
                .contains("residue of an earlier failed dispatch"),
            "got: {err:#}"
        );
        remove_worktree(&rig, &wt).unwrap();
        assert!(!wt.exists());
    }
}

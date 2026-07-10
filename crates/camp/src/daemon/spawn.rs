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

/// The ONE worker-contract source (dispatch-lifecycle Phase 3, Q5): the
/// worker skill shipped by the plugin. Embedded at compile time so the
/// mechanical floor campd injects and the skill a plugin user reads can
/// never drift — obligation (v) pins the equality by test.
const WORKER_SKILL: &str = include_str!("../../../../plugin/skills/worker/SKILL.md");

/// The skill body: frontmatter stripped. The skill uses `<bead>`/`<name>`
/// placeholders as documentation; task_prompt binds them per spawn.
fn skill_body() -> String {
    let mut lines = WORKER_SKILL.lines();
    // A malformed skill (no frontmatter fence) is a build defect, caught
    // by the tests below — fall through to the full text rather than
    // panicking in library code.
    if lines.next() != Some("---") {
        return WORKER_SKILL.to_owned();
    }
    lines
        .skip_while(|l| *l != "---")
        .skip(1)
        .collect::<Vec<_>>()
        .join("\n")
}

fn task_prompt(bead_id: &str, session_name: &str) -> String {
    let bound = skill_body()
        .replace("<bead>", bead_id)
        .replace("<name>", session_name);
    // .trim(): symmetric with the obligation-(v) test's expectation (N4) —
    // leading/trailing blank lines around the body never desynchronize the
    // "prompt ends with the transformed body" equality.
    format!(
        "You are Gas Camp worker session {session_name}, dispatched to work exactly one bead: {bead_id}. \
         CAMP_DIR is already set for the camp CLI; do not start unrelated work.\n\n{}",
        bound.trim()
    )
}

/// Every non-ASCII-alphanumeric CHARACTER becomes one '-' — Claude Code's
/// project dir scheme (F3), reused for the sessions/ capture file names.
/// Unicode (PR #14 review finding 6, resolved): verified per-CHAR against
/// real claude 2.1.204 (Phase 11 probe P1,
/// docs/design/2026-07-07-phase-11-probe-findings.md) — a multi-byte char
/// maps to a single dash in the real project dir too.
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

/// How the worker's stdin is wired (Decision C, probe P2/P3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdinMode {
    /// /dev/null (F5): the json-envelope mode, task via `-p`.
    Null,
    /// A pipe campd holds for the worker's lifetime: stream-json input
    /// mode. The task is the first `user_message` line; patrol nudges are
    /// later lines; dropping the write end starts the release path (an
    /// idle stream worker does NOT exit on EOF — probe P3 — so a release
    /// grace timer bounds the linger).
    HeldStream,
}

/// One stream-json user turn as claude accepts it (probe P2), newline
/// terminated: `{"type":"user","message":{"role":"user","content":<text>}}`.
pub fn user_message(text: &str) -> String {
    let mut line = serde_json::json!({
        "type": "user",
        "message": {"role": "user", "content": text},
    })
    .to_string();
    line.push('\n');
    line
}

/// F7 pins as recorded on the session's woke event — the values the resume
/// paths re-apply (issue #48 finding 1, resolved in dispatch-lifecycle
/// Phase 3; the decision record is the plan doc + spec §8.4).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResumePins {
    pub model: Option<String>,
    pub permission_mode: Option<String>,
    pub allowed_tools: Option<String>,
}

/// The ONE resume argv vocabulary (`camp nudge` resume + patrol
/// nudge-resume): `-p --resume <sid> <text> --output-format json` plus the
/// recorded F7 pins. NOT --append-system-prompt: the conversation already
/// embodies the role prompt.
pub fn resume_argv(sid: &str, text: &str, pins: &ResumePins) -> Vec<OsString> {
    let mut argv: Vec<OsString> = ["-p", "--resume", sid, text, "--output-format", "json"]
        .iter()
        .map(OsString::from)
        .collect();
    let mut push = |flag: &str, value: &Option<String>| {
        if let Some(v) = value {
            argv.push(OsString::from(flag));
            argv.push(OsString::from(v));
        }
    };
    push("--model", &pins.model);
    push("--permission-mode", &pins.permission_mode);
    push("--allowedTools", &pins.allowed_tools);
    argv
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
    pub stdin_mode: StdinMode,
}

/// The worker-contract task text for this spawn — the first stream
/// message in HeldStream mode (the caller writes it after spawn).
pub fn task_message(bead_id: &str, session_name: &str) -> String {
    user_message(&task_prompt(bead_id, session_name))
}

/// Assemble the exec plan. Pure — no filesystem, no process. The argv is
/// asserted verbatim by tests against F1/F2/F7, plan decision L, and
/// (stream mode) probe P2.
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
    stdin_mode: StdinMode,
) -> SpawnSpec {
    let mut argv: Vec<OsString> = vec![command.as_os_str().to_owned()];
    {
        let mut arg = |s: &str| argv.push(OsString::from(s));
        match stdin_mode {
            StdinMode::Null => {
                arg("--output-format");
                arg("json"); // F2
            }
            StdinMode::HeldStream => {
                // P2: stream in requires stream out; both accepted with
                // the pinned flags at 2.1.204.
                arg("--output-format");
                arg("stream-json");
                arg("--input-format");
                arg("stream-json");
            }
        }
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
        if stdin_mode == StdinMode::Null {
            arg(&task_prompt(bead_id, session_name)); // the task
        }
        // HeldStream: NO positional task — the task is the first
        // user_message the dispatcher writes to the held pipe.
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
            (
                "CAMP_TRANSCRIPT".to_owned(),
                transcript_path.to_string_lossy().into_owned(),
            ),
        ],
        stdout_path: sessions_dir.join(format!("{file_stem}.json")),
        stderr_path: sessions_dir.join(format!("{file_stem}.log")),
        stdin_mode,
    }
}

/// Exec the worker. stdin is /dev/null in Null mode (F5 — an open
/// non-pipe stdin costs a 3 s sniff) or a campd-held pipe in HeldStream
/// mode (Decision C — the live nudge path; the caller takes
/// `child.stdin`). stdout/stderr go to the sessions/ capture files
/// (decision G; stream mode makes the .json capture stream-JSONL, one
/// event per line). The child is deliberately not waited here:
/// SIGCHLD-driven try_wait in the dispatcher reaps it, and workers
/// intentionally outlive a killed campd (adoption, spec §8.5; P3: EOF
/// does not kill a stream worker).
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
        .stdin(match spec.stdin_mode {
            StdinMode::Null => Stdio::null(), // F5
            StdinMode::HeldStream => Stdio::piped(),
        })
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    for (key, value) in &spec.env {
        cmd.env(key, value);
    }
    cmd.spawn()
        .with_context(|| format!("spawning {}", spec.argv[0].to_string_lossy()))
}

/// A worktree needs a base: a git repository whose HEAD resolves to a
/// commit (spec §12 fail-fast). Modern git (2.42+) auto-infers `--orphan`
/// on an unborn HEAD and would happily create a baseless worktree —
/// precisely the stranded-work hazard the dispatch contract forbids — so
/// the refusal is an explicit mechanical check, never delegated to
/// `git worktree add` failing. Also catches "not a git repository at all"
/// through the same rev-parse.
fn ensure_worktree_base(rig_path: &Path) -> Result<()> {
    let out = Command::new("git")
        .arg("-C")
        .arg(rig_path)
        .args(["rev-parse", "--verify", "HEAD^{commit}"])
        .output()
        .context("running git rev-parse")?;
    if !out.status.success() {
        bail!(
            "rig {} cannot host a worktree (no git repository with a base commit): {}",
            rig_path.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// The rig's base commit at this moment — `git rev-parse --verify
/// HEAD^{commit}` — or None when the rig has none (non-repo / unborn
/// HEAD). Recorded in session.woke as the dispatch-time `base`: the
/// mechanical reference the `camp close` shipped gate verifies descent
/// from (using the rig's LATER HEAD would let a live-tree worker on a
/// baseless rig self-certify its own dead-end commit as based).
pub fn rig_base(rig_path: &Path) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(rig_path)
        .args(["rev-parse", "--verify", "HEAD^{commit}"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (!sha.is_empty()).then_some(sha)
}

/// `git worktree add -b camp/<bead> <camp>/worktrees/<bead>` (decision H).
/// A pre-existing directory or branch fails fast — bead ids are unique and
/// Phase 8 never respawns a bead. A rig with no base commit is refused
/// before any side effect (spec §12 fail-fast).
pub fn create_worktree(rig_path: &Path, worktrees_dir: &Path, bead_id: &str) -> Result<PathBuf> {
    ensure_worktree_base(rig_path)?;
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

/// `create_worktree` with respawn reuse (Phase 11 plan Decision H): an
/// existing directory is REUSED iff it is a git worktree whose checked-out
/// branch is camp/<bead> — the same bead's earlier generation, partial
/// work preserved. Anything else keeps the fail-fast residue error.
pub fn ensure_worktree(rig_path: &Path, worktrees_dir: &Path, bead_id: &str) -> Result<PathBuf> {
    // Defense-in-depth (PR #52 review finding 1): the base check runs on
    // the REUSE path too. Reuse implies a prior base-checked creation,
    // but a rig whose repository was gutted since must still fail fast
    // here — never hand a worker a broken tree. (The create path checks
    // again inside create_worktree; one extra rev-parse is noise.)
    ensure_worktree_base(rig_path)?;
    let dir = worktrees_dir.join(bead_id);
    if !dir.exists() {
        return create_worktree(rig_path, worktrees_dir, bead_id);
    }
    if dir.join(".git").exists() {
        let out = Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(["branch", "--show-current"])
            .output()
            .context("running git branch --show-current")?;
        if out.status.success()
            && String::from_utf8_lossy(&out.stdout).trim() == format!("camp/{bead_id}")
        {
            return Ok(dir);
        }
    }
    bail!(
        "worktree {} already exists (may be residue of an earlier failed dispatch)",
        dir.display()
    );
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
            stall_after: None,
            prompt: "Implement with TDD.".into(),
        }
    }

    /// Obligation (v), dispatch-lifecycle Phase 3 (Q5): ONE worker-contract
    /// source. The task prompt every campd worker receives is the worker
    /// skill's body verbatim (frontmatter stripped, <bead>/<name> bound),
    /// behind a two-line mechanical preamble. The transform is recomputed
    /// here independently from the file, so a divergent second copy in Rust
    /// cannot survive this assertion.
    #[test]
    fn the_task_prompt_is_the_worker_skill_verbatim() {
        let skill = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../plugin/skills/worker/SKILL.md"),
        )
        .unwrap();
        // frontmatter: first line is "---", body starts after the next "---" line
        let mut lines = skill.lines();
        assert_eq!(
            lines.next(),
            Some("---"),
            "skill must open with frontmatter"
        );
        let body: String = lines
            .by_ref()
            .skip_while(|l| *l != "---")
            .skip(1)
            .collect::<Vec<_>>()
            .join("\n");
        // Symmetric trim on BOTH sides (plan-review r2, N4): the impl embeds
        // `bound.trim()`, so the expectation trims identically — a leading
        // blank line after the frontmatter fence cannot desynchronize them.
        let expected = body
            .replace("<bead>", "gc-9")
            .replace("<name>", "t/dev/9")
            .trim()
            .to_owned();
        let prompt = task_prompt("gc-9", "t/dev/9");
        assert!(
            prompt.ends_with(&expected),
            "prompt must end with the transformed skill body;\nprompt tail: {}",
            &prompt[prompt.len().saturating_sub(200)..]
        );
        let preamble = prompt.strip_suffix(expected.as_str()).unwrap();
        assert!(preamble.contains("gc-9") && preamble.contains("t/dev/9"));
        assert!(preamble.contains("CAMP_DIR"));
        assert!(
            preamble.lines().filter(|l| !l.trim().is_empty()).count() <= 2,
            "the preamble is mechanical binding only, not a second contract: {preamble:?}"
        );
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
            StdinMode::Null,
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
                (
                    "CAMP_TRANSCRIPT".to_owned(),
                    "/home/u/.claude/projects/-code-gc/x.jsonl".to_owned()
                ),
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
            stall_after: None,
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
            StdinMode::Null,
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
        // PR #14 review finding 6, resolved: munge is per CHAR — one dash
        // per non-ASCII-alphanumeric character, however many bytes it
        // takes — verified against real claude 2.1.204 (Phase 11 probe P1:
        // cwd basename `héllo-日本` → project dir segment `h-llo---`).
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

    /// A committed git repo to serve as a rig (shared by the worktree
    /// tests).
    fn git_rig(dir: &Path) -> PathBuf {
        let rig = dir.join("rig");
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
        rig
    }

    /// Worktree lifecycle against a real git repo (decision H).
    #[test]
    fn worktree_create_and_remove_round_trip() {
        let _spawning = crate::daemon::spawn_probe_guard();
        let dir = tempfile::tempdir().unwrap();
        let rig = git_rig(dir.path());
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

    /// The dispatch-time base (Phase 3, Q4): the mechanical fact "what commit
    /// was this rig on when the work was dispatched" — the reference the
    /// shipped gate verifies descent from. None on an unborn HEAD or a
    /// non-repo (the same shapes ensure_worktree_base refuses).
    #[test]
    fn rig_base_resolves_head_and_is_none_without_one() {
        let _spawning = crate::daemon::spawn_probe_guard();
        let dir = tempfile::tempdir().unwrap();
        let rig = git_rig(dir.path());
        let base = rig_base(&rig).expect("a committed rig has a base");
        assert_eq!(base.len(), 40, "full sha: {base}");

        let bare = dir.path().join("bare");
        std::fs::create_dir_all(&bare).unwrap();
        assert!(rig_base(&bare).is_none(), "not a repo");
        Command::new("git")
            .arg("-C")
            .arg(&bare)
            .args(["init", "-b", "main"])
            .output()
            .unwrap();
        assert!(rig_base(&bare).is_none(), "unborn HEAD");
    }

    /// Phase 2 (spec §12 fail-fast): a rig without a base commit cannot
    /// host a worktree. Modern git (2.42+) auto-infers `--orphan` on an
    /// unborn HEAD and would happily create a baseless worktree — the
    /// stranded-work hazard the dispatch contract forbids — so the check
    /// must be explicit, not delegated to `git worktree add` failing.
    /// Covers both obligation-(ii) shapes: git-init-only and not-a-repo.
    #[test]
    fn create_worktree_refuses_a_rig_without_a_base_commit() {
        let _spawning = crate::daemon::spawn_probe_guard();
        let dir = tempfile::tempdir().unwrap();
        let worktrees = dir.path().join("worktrees");

        // git-init-only: a repo with an unborn HEAD (no base commit)
        let baseless = dir.path().join("baseless");
        std::fs::create_dir_all(&baseless).unwrap();
        let out = Command::new("git")
            .arg("-C")
            .arg(&baseless)
            .args(["init", "-b", "main"])
            .output()
            .unwrap();
        assert!(out.status.success());
        let err = create_worktree(&baseless, &worktrees, "gc-1").unwrap_err();
        assert!(
            err.to_string().contains("cannot host a worktree"),
            "got: {err:#}"
        );
        assert!(!worktrees.join("gc-1").exists(), "no residue on refusal");
        let branches = Command::new("git")
            .arg("-C")
            .arg(&baseless)
            .args(["branch", "--list"])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&branches.stdout).trim(),
            "",
            "no camp/<bead> branch may be created on a baseless rig"
        );

        // not a git repository at all
        let plain = dir.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        let err = create_worktree(&plain, &worktrees, "gc-2").unwrap_err();
        assert!(
            err.to_string().contains("cannot host a worktree"),
            "got: {err:#}"
        );
        assert!(!worktrees.join("gc-2").exists(), "no residue on refusal");
    }

    // ---- Phase 11: stream mode (Decision C, probe P2) + worktree reuse ---

    /// The stream spawn argv, pinned against probe P2 and F1/F7/decision L.
    #[test]
    fn stream_argv_matches_probe_p2_and_the_fixture_facts() {
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
            StdinMode::HeldStream,
        );
        let argv: Vec<&str> = spec.argv.iter().map(|s| s.to_str().unwrap()).collect();
        assert_eq!(
            argv,
            vec![
                "claude",
                "--output-format",
                "stream-json",
                "--input-format",
                "stream-json",
                "--session-id",
                sid,
                "--model",
                "sonnet",
                "--permission-mode",
                "acceptEdits",
                "--allowedTools",
                "Read,Edit,Bash",
                "--append-system-prompt",
                "Implement with TDD.",
                "-p",
            ],
            "NO positional task in stream mode — the task is the first \
             user_message over stdin"
        );
        assert!(
            spec.env.contains(&(
                "CAMP_TRANSCRIPT".to_owned(),
                "/home/u/.claude/projects/-code-gc/x.jsonl".to_owned()
            )),
            "env: {:?}",
            spec.env
        );
        assert_eq!(spec.stdin_mode, StdinMode::HeldStream);
    }

    /// The nudge/task wire shape, pinned against probe P2.
    #[test]
    fn user_message_is_one_escaped_stream_json_line() {
        let line = user_message("say \"hi\"\nnow");
        assert!(line.ends_with('\n'));
        assert_eq!(line.matches('\n').count(), 1, "ONE line on the wire");
        let v: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(v["type"], "user");
        assert_eq!(v["message"]["role"], "user");
        assert_eq!(v["message"]["content"], "say \"hi\"\nnow");
    }

    /// F5 as amended by Decision C: Null mode keeps /dev/null stdin;
    /// HeldStream pipes it and the caller owns the write end.
    #[test]
    fn held_stream_spawn_pipes_stdin_and_null_spawn_does_not() {
        let _spawning = crate::daemon::spawn_probe_guard();
        let dir = tempfile::tempdir().unwrap();
        let spec_for = |mode: StdinMode| SpawnSpec {
            session_name: "t/dev/1".into(),
            claude_session_id: "sid".into(),
            transcript_path: dir.path().join("t.jsonl"),
            cwd: dir.path().to_path_buf(),
            argv: vec![OsString::from("cat")],
            env: vec![],
            stdout_path: dir.path().join("sessions/out.json"),
            stderr_path: dir.path().join("sessions/err.log"),
            stdin_mode: mode,
        };

        let mut held = spawn(&spec_for(StdinMode::HeldStream)).unwrap();
        let mut stdin = held.stdin.take().expect("HeldStream must pipe stdin");
        use std::io::Write as _;
        stdin.write_all(b"ping\n").unwrap();
        drop(stdin); // EOF: cat exits 0
        let status = held.wait().unwrap();
        assert!(status.success());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("sessions/out.json")).unwrap(),
            "ping\n",
            "the capture file still receives stdout"
        );

        let mut null = spawn(&spec_for(StdinMode::Null)).unwrap();
        assert!(null.stdin.is_none(), "Null mode keeps /dev/null (F5)");
        assert!(null.wait().unwrap().success());
    }

    /// Decision H: a patrol respawn reuses the bead's own worktree
    /// (partial work preserved); anything else keeps the residue error.
    #[test]
    fn ensure_worktree_reuses_the_beads_worktree_and_rejects_impostors() {
        let _spawning = crate::daemon::spawn_probe_guard();
        let dir = tempfile::tempdir().unwrap();
        let rig = git_rig(dir.path());
        let worktrees = dir.path().join("worktrees");

        // absent -> creates (parity with create_worktree)
        let wt = ensure_worktree(&rig, &worktrees, "gc-7").unwrap();
        assert!(wt.join(".git").exists());

        // existing valid worktree on camp/<bead> -> reused, work preserved
        std::fs::write(wt.join("partial.txt"), "half-done").unwrap();
        let again = ensure_worktree(&rig, &worktrees, "gc-7").unwrap();
        assert_eq!(again, wt);
        assert_eq!(
            std::fs::read_to_string(wt.join("partial.txt")).unwrap(),
            "half-done"
        );

        // a plain directory (not a worktree) -> the residue error, verbatim
        let imposter_dir = worktrees.join("gc-8");
        std::fs::create_dir_all(&imposter_dir).unwrap();
        let err = ensure_worktree(&rig, &worktrees, "gc-8").unwrap_err();
        assert!(
            err.to_string().contains("residue"),
            "plain dir must fail fast: {err:#}"
        );
    }

    /// Issue #48 finding 1 (DECIDED, Phase 3): a resume turn re-applies the F7
    /// pins recorded at spawn — a session keeps its birth capability envelope;
    /// resuming under ambient user settings would silently widen a pinned
    /// worker's tools. Pins absent (the operator's own registered session) =
    /// a bare resume: a recorded absence, not a fallback. The role prompt
    /// (--append-system-prompt) is NOT re-applied — the conversation already
    /// embodies it.
    #[test]
    fn resume_argv_reapplies_recorded_pins_and_only_those() {
        let pins = ResumePins {
            model: Some("sonnet".into()),
            permission_mode: Some("acceptEdits".into()),
            allowed_tools: Some("Read,Edit,Bash".into()),
        };
        let argv: Vec<String> = resume_argv("sid-1", "status?", &pins)
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            argv,
            vec![
                "-p",
                "--resume",
                "sid-1",
                "status?",
                "--output-format",
                "json",
                "--model",
                "sonnet",
                "--permission-mode",
                "acceptEdits",
                "--allowedTools",
                "Read,Edit,Bash",
            ]
        );
        let bare: Vec<String> = resume_argv("sid-1", "status?", &ResumePins::default())
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            bare,
            vec![
                "-p",
                "--resume",
                "sid-1",
                "status?",
                "--output-format",
                "json"
            ]
        );
    }

    /// PR #52 review finding 1 (defense-in-depth): the REUSE path checks
    /// the base too. Reuse implies a prior base-checked creation, but a
    /// rig whose repository was gutted since (.git deleted) must still
    /// fail fast — never hand a worker a broken tree.
    #[test]
    fn ensure_worktree_reuse_refuses_a_rig_gutted_since_creation() {
        let _spawning = crate::daemon::spawn_probe_guard();
        let dir = tempfile::tempdir().unwrap();
        let rig = git_rig(dir.path());
        let worktrees = dir.path().join("worktrees");
        let wt = ensure_worktree(&rig, &worktrees, "gc-9").unwrap();
        assert!(wt.join(".git").exists());

        // gut the rig: the worktree dir remains, the repository is gone
        std::fs::remove_dir_all(rig.join(".git")).unwrap();
        let err = ensure_worktree(&rig, &worktrees, "gc-9").unwrap_err();
        assert!(
            err.to_string().contains("cannot host a worktree"),
            "reuse on a gutted rig must fail the base check: {err:#}"
        );
    }
}

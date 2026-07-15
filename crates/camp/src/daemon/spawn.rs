//! Worker spawn mechanics (spec §8.4, §12). The Phase 2 fixture facts
//! F1–F7 (docs/design/2026-07-06-assumption-findings.md) are BINDING here:
//! F1 pre-assigned --session-id, F2 --output-format json, F3 transcript
//! path from the WORKER's cwd, F5 stdin at /dev/null, F7 per-agent pinning
//! flags. Everything in this module is mechanical; roles live in packs.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use camp_core::pack::AgentDef;

use super::bounded;

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
///
/// cp-3 (§5.3.1): a resume is a ONE-SHOT `--output-format json` turn whose stdin
/// is `Stdio::null()` (both callers — `cmd/nudge.rs` and patrol nudge-resume).
/// It has NO campd-held control plane, so it deliberately gets NO
/// `--permission-prompt-tool stdio`: routing a permission ask to a stdio control
/// plane that does not exist would make a resumed turn that hits a permission
/// prompt block forever waiting for a `control_response` on null stdin — the
/// exact "blocked forever" failure §5.3 exists to eliminate. The flag is a
/// HeldStream-only (dispatch) affordance; §5.3.1 scopes it to the spawn path.
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

/// cp-3 (§5.3.1): the `--permission-prompt-tool stdio` flag routes ONLY
/// decisions the CLI would otherwise ask about. A mode that can never ask
/// (`bypassPermissions`) gets NO flag — adding it would make the CLI refuse the
/// argv (the incoherent combo). An unrecognized mode cannot be classified, so it
/// is refused at spawn rather than guessed (invariant 5, fail fast).
///
/// Returns `Ok(Some("stdio"))` for an askable mode (`None` → the CLI default,
/// `"default"`, `"acceptEdits"`, `"plan"`), `Ok(None)` for `"bypassPermissions"`,
/// and `Err(..)` for any other string.
pub fn permission_prompt_flag(
    permission_mode: Option<&str>,
) -> Result<Option<&'static str>, String> {
    match permission_mode {
        None | Some("default") | Some("acceptEdits") | Some("plan") => Ok(Some("stdio")),
        Some("bypassPermissions") => Ok(None),
        Some(other) => Err(format!(
            "unknown --permission-mode {other:?}: camp cannot tell whether it can ask for a \
             permission decision, so it refuses to spawn rather than guess (control-plane §5.3.1)"
        )),
    }
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
) -> Result<SpawnSpec, String> {
    let mut argv: Vec<OsString> = vec![command.as_os_str().to_owned()];
    {
        let mut arg = |s: &str| argv.push(OsString::from(s));
        match stdin_mode {
            StdinMode::Null => {
                arg("--output-format");
                arg("json"); // F2
            }
            StdinMode::HeldStream => {
                // P2: stream in requires stream out. The shipped CLI
                // (2.1.205–2.1.208; re-verified against the 2.1.208 pin by
                // `make compat`'s negative control) hard-rejects `--print` + stream-json
                // output UNLESS `--verbose` is passed (#86): `verbose`
                // resolves flag -> settings -> false, so without the flag
                // dispatch dies at argv validation on every machine whose
                // ~/.claude/settings.json does not set it. The Agent SDK
                // hardcodes `--verbose` here for exactly this reason;
                // camp does too. Order mirrors the SDK:
                // --output-format stream-json --verbose --input-format stream-json.
                arg("--output-format");
                arg("stream-json");
                arg("--verbose");
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
        // cp-3 (§5.3.1): route permission decisions to campd's stdio control
        // plane — ONLY in HeldStream (dispatch) mode, and ONLY when the resolved
        // mode can ask. A Null-mode `json` spawn never streams a control plane;
        // bypassPermissions gets NO flag (adding it makes the CLI refuse the
        // argv); an unclassifiable mode is refused at spawn (fail fast).
        if stdin_mode == StdinMode::HeldStream
            && let Some(flag) = permission_prompt_flag(agent.permission_mode.as_deref())?
        {
            arg("--permission-prompt-tool");
            arg(flag);
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
    Ok(SpawnSpec {
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
            // compat §6.1 — the gc worker contract's environment (projection
            // #3 of the claimed bead row). The role-worker fragment reads
            // BEADS_ACTOR (its EXPECTED_ASSIGNEE), falling back through
            // GC_SESSION_NAME/GC_SESSION_ID — all three are the SESSION, which
            // is `claimed_by` and so equals `bd show`'s `assignee` (Task 1).
            ("BEADS_ACTOR".to_owned(), session_name.to_owned()),
            ("GC_SESSION_NAME".to_owned(), session_name.to_owned()),
            ("GC_SESSION_ID".to_owned(), session_name.to_owned()),
            // EXPECTED_ROUTE = GC_TEMPLATE/GC_AGENT = the qualified agent. In
            // production this equals the cooked route (both from resolve_agent),
            // so the fragment's route check (bd show gc.routed_to == env) holds;
            // the guard fixtures mismatch them on purpose to prove the shim
            // reads the route from the BEAD, not from here (round-1 B1).
            ("GC_AGENT".to_owned(), agent.name.clone()),
            ("GC_TEMPLATE".to_owned(), agent.name.clone()),
            // §6.3 — the worker's PATH resolves `gc`/`bd` to `.camp/bin` FIRST.
            // Only campd-dispatched workers get this; attended sessions do not.
            (
                "PATH".to_owned(),
                crate::cmd::shim::install::prepend_bin_path(
                    camp_root,
                    std::env::var("PATH").ok().as_deref(),
                ),
            ),
        ],
        stdout_path: sessions_dir.join(format!("{file_stem}.json")),
        stderr_path: sessions_dir.join(format!("{file_stem}.log")),
        stdin_mode,
    })
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
fn ensure_worktree_base(rig_path: &Path, timeout: Duration) -> Result<()> {
    let out = bounded::output_bounded(
        Command::new("git").arg("-C").arg(rig_path).args([
            "rev-parse",
            "--verify",
            "HEAD^{commit}",
        ]),
        timeout,
    )
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
/// HEAD^{commit}` — or None when the rig OBSERVABLY has none (non-repo /
/// unborn HEAD). Recorded in session.woke as the dispatch-time `base`:
/// the mechanical reference the `camp close` shipped gate verifies
/// descent from (using the rig's LATER HEAD would let a live-tree worker
/// on a baseless rig self-certify its own dead-end commit as based). A
/// git that cannot run or hangs past the bound is an Err (issue #55) —
/// mapping it to None would silently record a probe failure as a
/// baseless rig (invariant 5).
pub fn rig_base(rig_path: &Path, timeout: Duration) -> Result<Option<String>> {
    let out = bounded::output_bounded(
        Command::new("git").arg("-C").arg(rig_path).args([
            "rev-parse",
            "--verify",
            "HEAD^{commit}",
        ]),
        timeout,
    )
    .context("running git rev-parse")?;
    if !out.status.success() {
        return Ok(None);
    }
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    Ok((!sha.is_empty()).then_some(sha))
}

/// Detect a pre-existing `camp/<bead>` branch on `rig_path` and, if one is
/// there, build the loud named-recovery error dispatch needs (issue #82).
/// Bead ids are ledger-scoped but `camp/<bead>` branches are repo-permanent,
/// so a ledger reset restarts the bead counter onto a predecessor's branch;
/// `git worktree add -b` then dies on a raw "a branch named ... already
/// exists". The probe is EXPLICIT — the same discipline as
/// `ensure_worktree_base`: never delegate detection to `git worktree add`
/// failing. Returns `Ok(None)` when there is no such branch (the common,
/// happy path — one extra bounded rev-parse, "noise" per the module).
///
/// The message states whether the branch holds commits NOT reachable from
/// the rig's base (possible unpushed work) and names the exact recovery
/// command. It NEVER deletes the branch: a leftover branch may hold unpushed
/// work (`remove_worktree`'s own contract), so clearing it is the operator's
/// deliberate, informed act.
fn branch_collision_error(
    rig_path: &Path,
    bead_id: &str,
    timeout: Duration,
) -> Result<Option<String>> {
    let branch = format!("camp/{bead_id}");
    let refname = format!("refs/heads/{branch}");

    // Explicit existence probe: exit 0 + sha when present, nonzero when not.
    let exists = bounded::output_bounded(
        Command::new("git")
            .arg("-C")
            .arg(rig_path)
            .args(["rev-parse", "--verify", "--quiet"])
            .arg(&refname),
        timeout,
    )
    .context("running git rev-parse for the branch-collision probe")?;
    if !exists.status.success() {
        return Ok(None); // no camp/<bead> branch: no collision
    }

    // The branch exists. The base is guaranteed present here (create_worktree
    // runs ensure_worktree_base first); a None now means the repo was gutted
    // between the two probes — fail loud rather than guess (invariant 5).
    let base = rig_base(rig_path, timeout)?
        .context("rig carries a camp/<bead> branch but has no base commit to compare it against")?;

    // Commits on the branch not reachable from the base: >0 means possible
    // unpushed work the operator must not lose.
    let counted = bounded::output_bounded(
        Command::new("git")
            .arg("-C")
            .arg(rig_path)
            .args(["rev-list", "--count"])
            .arg(&refname)
            .arg("--not")
            .arg(&base),
        timeout,
    )
    .context("running git rev-list for the branch-collision probe")?;
    if !counted.status.success() {
        bail!(
            "git rev-list failed inspecting branch {branch}: {}",
            String::from_utf8_lossy(&counted.stderr).trim()
        );
    }
    let ahead: u64 = String::from_utf8_lossy(&counted.stdout)
        .trim()
        .parse()
        .context("parsing git rev-list --count output")?;

    let rig = rig_path.display();
    let delete = format!("git -C {rig} branch -D {branch}");
    let message = if ahead == 0 {
        format!(
            "cannot dispatch bead {bead_id}: git branch {branch} already exists on rig {rig} \
             and holds no commits beyond the rig's base — it is leftover residue. Bead ids are \
             ledger-scoped but camp/<bead> branches are repo-permanent, so a ledger reset \
             restarts the bead counter onto a predecessor's branch. It is safe to delete: \
             {delete}, then re-dispatch."
        )
    } else {
        let inspect = format!("git -C {rig} log {base}..{branch}");
        format!(
            "cannot dispatch bead {bead_id}: git branch {branch} already exists on rig {rig} \
             and holds {ahead} commit(s) not on the rig's base {base} — it may contain unpushed \
             work. Bead ids are ledger-scoped but camp/<bead> branches are repo-permanent, so a \
             ledger reset restarts the bead counter onto a predecessor's branch. Inspect it \
             ({inspect}); once anything you need is preserved, delete it ({delete}) before \
             re-dispatching."
        )
    };
    Ok(Some(message))
}

/// `git worktree add -b camp/<bead> <camp>/worktrees/<bead>` (decision H).
/// A pre-existing directory fails fast (residue hint below; this check runs
/// FIRST — with the directory present the branch is typically checked out
/// there, so the collision error's advice would not apply). A pre-existing
/// `camp/<bead>` branch with no directory fails fast too, but with an
/// actionable named-recovery error (issue #82): bead ids are ledger-scoped
/// while branches are repo-permanent, so "bead ids are unique" is false
/// across a ledger reset — the error names the branch, says whether it
/// holds commits beyond the base, and gives the exact command to clear it
/// (the branch is never deleted here — it may hold unpushed work). A rig
/// with no base commit is refused before any side effect (spec §12
/// fail-fast); every refusal above is side-effect-free.
pub fn create_worktree(
    rig_path: &Path,
    worktrees_dir: &Path,
    bead_id: &str,
    timeout: Duration,
) -> Result<PathBuf> {
    ensure_worktree_base(rig_path, timeout)?;
    let dir = worktrees_dir.join(bead_id);
    if dir.exists() {
        // The residue hint matters (PR #14 review finding 4): this branch
        // also fires when a session.woke append failed after the worktree
        // was created, and the message must not hide that history. It runs
        // BEFORE the branch-collision probe: with the directory present the
        // camp/<bead> branch is typically checked out in that live worktree,
        // where the collision error's `branch -D` advice would be wrong.
        bail!(
            "worktree {} already exists (may be residue of an earlier failed dispatch)",
            dir.display()
        );
    }
    // Issue #82: refuse a repo-permanent camp/<bead> branch left by a
    // previous ledger's life BEFORE any side effect, with an actionable
    // named-recovery error instead of the raw `git worktree add` stderr
    // that used to permanently wedge dispatch on a reset camp's bead #1.
    if let Some(message) = branch_collision_error(rig_path, bead_id, timeout)? {
        bail!(message);
    }
    std::fs::create_dir_all(worktrees_dir)
        .with_context(|| format!("creating {}", worktrees_dir.display()))?;
    let out = bounded::output_bounded(
        Command::new("git")
            .arg("-C")
            .arg(rig_path)
            .args(["worktree", "add", "-b"])
            .arg(format!("camp/{bead_id}"))
            .arg(&dir),
        timeout,
    )
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
pub fn ensure_worktree(
    rig_path: &Path,
    worktrees_dir: &Path,
    bead_id: &str,
    timeout: Duration,
) -> Result<PathBuf> {
    // Defense-in-depth (PR #52 review finding 1): the base check runs on
    // the REUSE path too. Reuse implies a prior base-checked creation,
    // but a rig whose repository was gutted since must still fail fast
    // here — never hand a worker a broken tree. (The create path checks
    // again inside create_worktree; one extra rev-parse is noise.)
    ensure_worktree_base(rig_path, timeout)?;
    let dir = worktrees_dir.join(bead_id);
    if !dir.exists() {
        return create_worktree(rig_path, worktrees_dir, bead_id, timeout);
    }
    if dir.join(".git").exists() {
        let out = bounded::output_bounded(
            Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(["branch", "--show-current"]),
            timeout,
        )
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
pub fn remove_worktree(rig_path: &Path, worktree: &Path, timeout: Duration) -> Result<()> {
    let out = bounded::output_bounded(
        Command::new("git")
            .arg("-C")
            .arg(rig_path)
            .args(["worktree", "remove", "--force"])
            .arg(worktree),
        timeout,
    )
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

    /// Generous test bound: these fixtures exercise git semantics, not
    /// the deadline (bounded.rs pins the deadline behavior).
    const TEST_EXEC_TIMEOUT: Duration = Duration::from_secs(30);

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
        )
        .unwrap();
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

        // The four CAMP_* vars, then the compat §6.1 gc-worker environment
        // (five gc vars + PATH). PATH is inherited-dependent, so assert its
        // prefix separately.
        assert_eq!(
            spec.env[..9].to_vec(),
            vec![
                ("CAMP_DIR".to_owned(), "/camps/dev".to_owned()),
                ("CAMP_BEAD".to_owned(), "gc-142".to_owned()),
                ("CAMP_SESSION".to_owned(), "dev/dev/1".to_owned()),
                (
                    "CAMP_TRANSCRIPT".to_owned(),
                    "/home/u/.claude/projects/-code-gc/x.jsonl".to_owned()
                ),
                ("BEADS_ACTOR".to_owned(), "dev/dev/1".to_owned()),
                ("GC_SESSION_NAME".to_owned(), "dev/dev/1".to_owned()),
                ("GC_SESSION_ID".to_owned(), "dev/dev/1".to_owned()),
                ("GC_AGENT".to_owned(), "dev".to_owned()),
                ("GC_TEMPLATE".to_owned(), "dev".to_owned()),
            ]
        );
        assert_eq!(spec.env[9].0, "PATH");
        assert!(
            spec.env[9].1.starts_with("/camps/dev/bin:"),
            "PATH: {}",
            spec.env[9].1
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

    /// compat §6.1 — the worker environment is the third projection of the
    /// claimed bead row. All three assignee-chain vars are the SESSION; both
    /// route vars are the qualified AGENT; PATH prepends `.camp/bin`.
    #[test]
    fn build_spec_exports_the_gc_worker_environment() {
        let agent = AgentDef {
            name: "gc.run-operator".into(),
            ..full_agent()
        };
        let spec = build_spec(
            Path::new("claude"),
            &agent,
            Path::new("/camps/dev"),
            "gc-142",
            "dev/gc.run-operator/1",
            "sid",
            Path::new("/h/.claude/x.jsonl"),
            Path::new("/code/gc"),
            StdinMode::HeldStream,
        )
        .unwrap();
        let env: std::collections::BTreeMap<_, _> = spec.env.iter().cloned().collect();
        for k in ["BEADS_ACTOR", "GC_SESSION_NAME", "GC_SESSION_ID"] {
            assert_eq!(env[k], "dev/gc.run-operator/1", "{k}");
        }
        for k in ["GC_AGENT", "GC_TEMPLATE"] {
            assert_eq!(env[k], "gc.run-operator", "{k}");
        }
        assert!(
            env["PATH"].starts_with("/camps/dev/bin:"),
            "PATH: {}",
            env["PATH"]
        );
        assert_eq!(env["CAMP_BEAD"], "gc-142"); // the four CAMP_* still present
        assert_eq!(env["CAMP_SESSION"], "dev/gc.run-operator/1");
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
        )
        .unwrap();
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
        let wt = create_worktree(&rig, &worktrees, "gc-7", TEST_EXEC_TIMEOUT).unwrap();
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
        let err = create_worktree(&rig, &worktrees, "gc-7", TEST_EXEC_TIMEOUT).unwrap_err();
        assert!(
            err.to_string()
                .contains("residue of an earlier failed dispatch"),
            "got: {err:#}"
        );
        remove_worktree(&rig, &wt, TEST_EXEC_TIMEOUT).unwrap();
        assert!(!wt.exists());
    }

    /// Issue #82: a leftover `camp/<bead>` branch (predecessor ledger's
    /// residue — the worktrees dir was deleted with the ledger, the
    /// repo-permanent branch survived) must NOT die on raw git stderr. It
    /// fails with a loud, self-explaining, named-recovery error and the
    /// branch is left untouched (silent deletion is forbidden — it may hold
    /// unpushed work).
    #[test]
    fn create_worktree_refuses_a_stale_predecessor_branch_with_an_actionable_error() {
        let _spawning = crate::daemon::spawn_probe_guard();
        let dir = tempfile::tempdir().unwrap();
        let rig = git_rig(dir.path());
        let worktrees = dir.path().join("worktrees");

        // Predecessor residue: the branch exists at the rig's base, no
        // worktree directory (the reset scenario). git branch <name> makes
        // a branch at HEAD without checking it out or creating a worktree.
        let out = Command::new("git")
            .arg("-C")
            .arg(&rig)
            .args(["branch", "camp/gc-1"])
            .output()
            .unwrap();
        assert!(out.status.success(), "seeding the stale branch");

        let err = create_worktree(&rig, &worktrees, "gc-1", TEST_EXEC_TIMEOUT).unwrap_err();
        let msg = format!("{err:#}");

        // names the branch, states it is safe (no commits beyond base), and
        // gives the exact recovery command
        assert!(msg.contains("camp/gc-1"), "must name the branch: {msg}");
        assert!(
            msg.contains("already exists"),
            "must state the collision: {msg}"
        );
        assert!(
            msg.contains("safe to delete"),
            "no-unique-commits branch is safe: {msg}"
        );
        assert!(
            msg.contains(&format!("git -C {} branch -D camp/gc-1", rig.display())),
            "must name the exact recovery command: {msg}"
        );
        // explains WHY (ledger-scoped ids vs repo-permanent branches)
        assert!(
            msg.contains("ledger") && msg.contains("repo-permanent"),
            "must explain the structural cause: {msg}"
        );

        // NO silent deletion: the branch still exists after the refusal
        let still = Command::new("git")
            .arg("-C")
            .arg(&rig)
            .args(["rev-parse", "--verify", "--quiet", "refs/heads/camp/gc-1"])
            .output()
            .unwrap();
        assert!(still.status.success(), "the branch must NOT be deleted");
        // no worktree residue created on the refusal
        assert!(
            !worktrees.join("gc-1").exists(),
            "no worktree dir on refusal"
        );
    }

    /// Issue #82: when the stale branch holds commits NOT on the rig's base,
    /// it may carry unpushed work — the error says so, gives the inspect
    /// command, and still names the delete command; the branch is preserved.
    #[test]
    fn create_worktree_flags_a_stale_branch_that_holds_unpushed_work() {
        let _spawning = crate::daemon::spawn_probe_guard();
        let dir = tempfile::tempdir().unwrap();
        let rig = git_rig(dir.path());
        let worktrees = dir.path().join("worktrees");

        // A stale branch with a unique commit beyond the base: add the
        // branch via a throwaway worktree, commit onto it, then remove the
        // worktree (leaving the branch — the repo-permanent residue).
        let stale = dir.path().join("stale");
        let out = Command::new("git")
            .arg("-C")
            .arg(&rig)
            .args(["worktree", "add", "-b", "camp/gc-1"])
            .arg(&stale)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git worktree add: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let out = Command::new("git")
            .arg("-C")
            .arg(&stale)
            .args(["commit", "--allow-empty", "-m", "unpushed work"])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git commit: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        // remove the throwaway worktree; the branch (with its commit) remains
        let out = Command::new("git")
            .arg("-C")
            .arg(&rig)
            .args(["worktree", "remove", "--force"])
            .arg(&stale)
            .output()
            .unwrap();
        assert!(out.status.success(), "removing the throwaway worktree");

        let err = create_worktree(&rig, &worktrees, "gc-1", TEST_EXEC_TIMEOUT).unwrap_err();
        let msg = format!("{err:#}");

        assert!(msg.contains("camp/gc-1"), "must name the branch: {msg}");
        assert!(
            msg.contains("unpushed work"),
            "must warn about unpushed work: {msg}"
        );
        assert!(
            msg.contains(&format!("git -C {} log", rig.display())) && msg.contains("..camp/gc-1"),
            "must give the inspect command: {msg}"
        );
        assert!(
            msg.contains(&format!("git -C {} branch -D camp/gc-1", rig.display())),
            "must still name the delete command: {msg}"
        );

        // branch preserved (its commit is not lost)
        let still = Command::new("git")
            .arg("-C")
            .arg(&rig)
            .args(["rev-parse", "--verify", "--quiet", "refs/heads/camp/gc-1"])
            .output()
            .unwrap();
        assert!(still.status.success(), "the branch must NOT be deleted");
        assert!(
            !worktrees.join("gc-1").exists(),
            "no worktree dir on refusal"
        );
    }

    /// Issue #82 via the real dispatch entry point: dispatch::launch calls
    /// ensure_worktree, which on the reset scenario (worktrees dir absent,
    /// branch present) delegates to create_worktree — so the actionable
    /// error surfaces there too, and flows into dispatch.failed unchanged.
    #[test]
    fn ensure_worktree_surfaces_the_branch_collision_on_the_create_path() {
        let _spawning = crate::daemon::spawn_probe_guard();
        let dir = tempfile::tempdir().unwrap();
        let rig = git_rig(dir.path());
        let worktrees = dir.path().join("worktrees");

        let out = Command::new("git")
            .arg("-C")
            .arg(&rig)
            .args(["branch", "camp/gc-1"])
            .output()
            .unwrap();
        assert!(out.status.success());

        let err = ensure_worktree(&rig, &worktrees, "gc-1", TEST_EXEC_TIMEOUT).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("camp/gc-1") && msg.contains("already exists"),
            "got: {msg}"
        );
        assert!(
            msg.contains(&format!("git -C {} branch -D camp/gc-1", rig.display())),
            "must name the recovery command: {msg}"
        );
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
        let base = rig_base(&rig, TEST_EXEC_TIMEOUT)
            .unwrap()
            .expect("a committed rig has a base");
        assert_eq!(base.len(), 40, "full sha: {base}");

        let bare = dir.path().join("bare");
        std::fs::create_dir_all(&bare).unwrap();
        assert!(
            rig_base(&bare, TEST_EXEC_TIMEOUT).unwrap().is_none(),
            "not a repo"
        );
        Command::new("git")
            .arg("-C")
            .arg(&bare)
            .args(["init", "-b", "main"])
            .output()
            .unwrap();
        assert!(
            rig_base(&bare, TEST_EXEC_TIMEOUT).unwrap().is_none(),
            "unborn HEAD"
        );
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
        let err = create_worktree(&baseless, &worktrees, "gc-1", TEST_EXEC_TIMEOUT).unwrap_err();
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
        let err = create_worktree(&plain, &worktrees, "gc-2", TEST_EXEC_TIMEOUT).unwrap_err();
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
        )
        .unwrap();
        let argv: Vec<&str> = spec.argv.iter().map(|s| s.to_str().unwrap()).collect();
        assert_eq!(
            argv,
            vec![
                "claude",
                "--output-format",
                "stream-json",
                "--verbose",
                "--input-format",
                "stream-json",
                "--session-id",
                sid,
                "--model",
                "sonnet",
                "--permission-mode",
                "acceptEdits",
                // cp-3 (§5.3.1): HeldStream + an askable mode routes decisions to
                // campd's stdio control plane. Placed in the shared tail after
                // --permission-mode; cp-4's --include-partial-messages sits in the
                // stream-flags arm, a distinct position.
                "--permission-prompt-tool",
                "stdio",
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

    /// cp-3 (§5.3.1): the `--permission-prompt-tool stdio` flag rides ONLY an
    /// askable mode in HeldStream; bypassPermissions gets no flag; an
    /// unclassifiable mode is refused at spawn.
    #[test]
    fn permission_prompt_flag_is_added_only_for_askable_modes() {
        fn argv_for(mode: Option<&str>) -> Vec<String> {
            let agent = AgentDef {
                name: "a".into(),
                model: None,
                tools: None,
                permission_mode: mode.map(str::to_owned),
                isolation: Isolation::None,
                stall_after: None,
                prompt: "P".into(),
            };
            let spec = build_spec(
                Path::new("claude"),
                &agent,
                Path::new("/c"),
                "gc-1",
                "t/a/1",
                "sid",
                Path::new("/t.jsonl"),
                Path::new("/code"),
                StdinMode::HeldStream,
            )
            .unwrap();
            spec.argv
                .iter()
                .map(|s| s.to_string_lossy().into_owned())
                .collect()
        }
        fn pair_present(argv: &[String], flag: &str, value: &str) -> bool {
            argv.windows(2).any(|w| w[0] == flag && w[1] == value)
        }

        for mode in [None, Some("default"), Some("acceptEdits"), Some("plan")] {
            let argv = argv_for(mode);
            assert!(
                pair_present(&argv, "--permission-prompt-tool", "stdio"),
                "mode {mode:?} can ask → the stdio flag routes its decisions: {argv:?}"
            );
        }
        let argv = argv_for(Some("bypassPermissions"));
        assert!(
            !argv.iter().any(|a| a == "--permission-prompt-tool"),
            "bypassPermissions never asks → no flag, no behaviour change: {argv:?}"
        );
        assert!(
            permission_prompt_flag(Some("wat")).is_err(),
            "an unclassifiable mode is refused, never guessed (invariant 5)"
        );
    }

    /// cp-3 (§5.3.1): a Null-mode (json) spawn never streams a control plane, so
    /// it gets NO permission-prompt-tool flag regardless of mode.
    #[test]
    fn null_mode_never_gets_the_permission_prompt_flag() {
        let agent = AgentDef {
            name: "a".into(),
            model: None,
            tools: None,
            permission_mode: Some("acceptEdits".into()),
            isolation: Isolation::None,
            stall_after: None,
            prompt: "P".into(),
        };
        let spec = build_spec(
            Path::new("claude"),
            &agent,
            Path::new("/c"),
            "gc-1",
            "t/a/1",
            "sid",
            Path::new("/t.jsonl"),
            Path::new("/code"),
            StdinMode::Null,
        )
        .unwrap();
        let argv: Vec<&str> = spec.argv.iter().map(|s| s.to_str().unwrap()).collect();
        assert!(
            !argv.contains(&"--permission-prompt-tool"),
            "Null mode has no stdio control plane: {argv:?}"
        );
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
        let wt = ensure_worktree(&rig, &worktrees, "gc-7", TEST_EXEC_TIMEOUT).unwrap();
        assert!(wt.join(".git").exists());

        // existing valid worktree on camp/<bead> -> reused, work preserved
        std::fs::write(wt.join("partial.txt"), "half-done").unwrap();
        let again = ensure_worktree(&rig, &worktrees, "gc-7", TEST_EXEC_TIMEOUT).unwrap();
        assert_eq!(again, wt);
        assert_eq!(
            std::fs::read_to_string(wt.join("partial.txt")).unwrap(),
            "half-done"
        );

        // a plain directory (not a worktree) -> the residue error, verbatim
        let imposter_dir = worktrees.join("gc-8");
        std::fs::create_dir_all(&imposter_dir).unwrap();
        let err = ensure_worktree(&rig, &worktrees, "gc-8", TEST_EXEC_TIMEOUT).unwrap_err();
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

    /// cp-3 (§5.3.1): a resume is a null-stdin one-shot with NO campd control
    /// plane, so `resume_argv` must NEVER carry `--permission-prompt-tool` — not
    /// even for an askable mode. Routing a permission ask to a stdio control
    /// plane that does not exist would hang a resumed turn forever on null stdin
    /// (the very failure §5.3 eliminates). The flag is HeldStream-only.
    #[test]
    fn resume_argv_never_carries_the_permission_prompt_flag() {
        for mode in [None, Some("default"), Some("acceptEdits"), Some("plan")] {
            let pins = ResumePins {
                model: None,
                permission_mode: mode.map(str::to_owned),
                allowed_tools: None,
            };
            let argv: Vec<String> = resume_argv("sid-1", "go", &pins)
                .iter()
                .map(|s| s.to_string_lossy().into_owned())
                .collect();
            assert!(
                !argv.iter().any(|a| a == "--permission-prompt-tool"),
                "a resume ({mode:?}) has no stdio control plane — the flag would hang it: {argv:?}"
            );
        }
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
        let wt = ensure_worktree(&rig, &worktrees, "gc-9", TEST_EXEC_TIMEOUT).unwrap();
        assert!(wt.join(".git").exists());

        // gut the rig: the worktree dir remains, the repository is gone
        std::fs::remove_dir_all(rig.join(".git")).unwrap();
        let err = ensure_worktree(&rig, &worktrees, "gc-9", TEST_EXEC_TIMEOUT).unwrap_err();
        assert!(
            err.to_string().contains("cannot host a worktree"),
            "reuse on a gutted rig must fail the base check: {err:#}"
        );
    }
}

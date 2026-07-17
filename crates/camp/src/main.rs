#![forbid(unsafe_code)]

mod campdir;
mod daemon;
mod gitignore;
mod service;
mod cmd {
    pub mod adopt;
    pub mod attach;
    pub mod backup;
    pub mod claim;
    pub mod close;
    pub mod create;
    pub mod decide;
    pub mod doctor;
    pub mod event_emit;
    pub mod events;
    pub mod export;
    pub mod import;
    pub mod init;
    pub mod interrupt;
    pub mod ls;
    pub mod mail;
    pub mod nudge;
    pub mod order;
    pub mod recall;
    pub mod remember;
    pub mod retry;
    pub mod rig;
    pub mod search;
    pub mod service;
    pub mod session;
    pub mod sessions;
    pub mod shim;
    pub mod show;
    pub mod sling;
    pub mod stop;
    pub mod top;
    pub mod watch;
}

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use campdir::CampDir;
use service::ServiceChoice;

#[derive(Parser)]
#[command(
    name = "camp",
    version,
    about = "Gas Camp: durable agent work, one SQLite ledger, zero idle cost",
    arg_required_else_help = true
)]
struct Cli {
    /// Camp directory (default: $CAMP_DIR, else walk up from cwd for .camp/)
    #[arg(long, global = true, value_name = "DIR")]
    camp: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a new camp (./.camp by default; --camp DIR to choose the place)
    Init {
        /// Install and start a host service unit; a hard error when no host
        /// service manager is available (container/CI)
        #[arg(long, conflicts_with = "no_service")]
        service: bool,
        /// Do not install a host service unit (containers, CI, or by choice)
        #[arg(long = "no-service")]
        no_service: bool,
        /// An existing camp is a no-op success, not an error — for entrypoints
        /// and units that re-run `camp init` on every start (contrib/docker/).
        /// A no-op, never a repair, so it contradicts --service: to put an
        /// existing camp under a supervisor, `camp service install`
        #[arg(long = "exists-ok", conflicts_with = "service")]
        exists_ok: bool,
        /// Import a starter pack from this source (no prompt; composes with
        /// --exists-ok). A local path or a git/file URL.
        #[arg(long, conflicts_with = "no_import")]
        import: Option<String>,
        /// Skip the starter-pack prompt/offer entirely.
        #[arg(long = "no-import", conflicts_with = "import")]
        no_import: bool,
    },
    /// Verify ledger invariants
    #[command(group(
        clap::ArgGroup::new("mode").required(true).args(["refold", "formula", "drain_reservations", "orphan_runs"])
    ))]
    Doctor {
        /// Rebuild state from the event log and report drift (spec §13.5)
        #[arg(long)]
        refold: bool,
        /// Replace the state tables with the refolded content
        #[arg(long, requires = "refold")]
        repair: bool,
        /// Validate a formula file against the camp subset (spec §8.2)
        #[arg(long, value_name = "PATH", conflicts_with = "refold")]
        formula: Option<PathBuf>,
        /// Machine-readable verdict. Exits 0 even when the formula does not load
        /// — the VERDICT is the output, not the exit code (the §10 gate reads it).
        #[arg(long, requires = "formula")]
        json: bool,
        /// List exclusive drain reservations, flagging ORPHANS (a reservation
        /// whose holding anchor is closed or gone — a kill -9 between the reserve
        /// batch and the cook).
        #[arg(long, conflicts_with_all = ["refold", "formula"])]
        drain_reservations: bool,
        /// Release the orphans `--drain-reservations` finds. The operator escape:
        /// a held member no drain will ever gather is a member no drain can ever
        /// take.
        #[arg(long, requires = "drain_reservations")]
        release_orphans: bool,
        /// List `runs/<id>/` directories no `run.cooked` event names — the
        /// leftovers of a kill -9 inside cook's files-before-ledger window
        /// (#124). READ-ONLY: listing never deletes.
        #[arg(long, conflicts_with_all = ["refold", "formula", "drain_reservations"])]
        orphan_runs: bool,
        /// Remove the orphans `--orphan-runs` finds. Refuses while campd is
        /// running, and never touches a directory recently written to — that is
        /// what a healthy in-flight cook looks like.
        #[arg(long, requires = "orphan_runs")]
        sweep_orphan_runs: bool,
        /// Emit camp's COMPILED steps in the differential gate's normalized shape
        /// (`ci/gc-compat/differential.py` diffs them against gc's real compiler).
        #[arg(long, requires = "formula")]
        compiled: bool,
    },
    /// Append events by hand (worker contract surface)
    Event {
        #[command(subcommand)]
        command: EventCommand,
    },
    /// Print events from the ledger
    Events {
        /// Emit canonical JSONL (spec §7.2)
        #[arg(long)]
        json: bool,
        /// First seq to include (default 1)
        #[arg(long)]
        from: Option<i64>,
        /// Last seq to include (default: latest)
        #[arg(long)]
        to: Option<i64>,
    },
    /// Manage rigs (registered repositories)
    Rig {
        #[command(subcommand)]
        command: RigCommand,
    },
    /// Create a bead in the ledger
    Create {
        /// Bead title
        title: String,
        /// Rig (default: the only configured rig)
        #[arg(long)]
        rig: Option<String>,
        /// Longer description
        #[arg(long)]
        description: Option<String>,
        /// A bead this one depends on (repeatable)
        #[arg(long = "needs")]
        needs: Vec<String>,
        /// A label (repeatable)
        #[arg(long = "label")]
        labels: Vec<String>,
        /// Bead type (task|mail|memory; default task)
        #[arg(long = "type")]
        bead_type: Option<String>,
        /// Routing hint to a pack agent
        #[arg(long)]
        assignee: Option<String>,
        /// Add this bead to a run as a MEMBER (compat §9 D3): a drain step in
        /// that run scatters one item run per member. The run must exist.
        #[arg(long)]
        run: Option<String>,
    },
    /// Claim a bead for a session (open → in_progress)
    Claim {
        /// Bead id
        bead: String,
        /// Claiming session name
        #[arg(long)]
        session: String,
    },
    /// Close a bead with an outcome
    Close {
        /// Bead id
        bead: String,
        /// Outcome
        #[arg(long, value_parser = ["pass", "fail"])]
        outcome: String,
        /// Close note (searchable)
        #[arg(long)]
        reason: Option<String>,
        /// Classify this failure as transient (retry vocabulary, spec §8.2)
        #[arg(long)]
        transient: bool,
        /// Structured step output: a JSON file path, or "-" for stdin
        #[arg(long, value_name = "FILE")]
        output_json: Option<String>,
        /// Work outcome (gc's WorkOutcome axis, verbatim): what became of
        /// the work itself — separate from the control outcome
        #[arg(long, value_parser = ["shipped", "no-op", "blocked", "abandoned"])]
        work_outcome: Option<String>,
        /// The commit that satisfies the bead (required with --work-outcome shipped)
        #[arg(long, value_name = "SHA")]
        work_commit: Option<String>,
        /// The branch the commit lives on (required with --work-outcome shipped)
        #[arg(long, value_name = "BRANCH")]
        work_branch: Option<String>,
    },
    /// List beads
    Ls {
        /// Only open, unblocked beads
        #[arg(long, conflicts_with = "mine")]
        ready: bool,
        /// Only beads claimed by this session
        #[arg(long)]
        mine: Option<String>,
        /// Scope to a rig
        #[arg(long)]
        rig: Option<String>,
        /// Emit JSON
        #[arg(long)]
        json: bool,
    },
    /// Sling work: a bare title (Tier 0) or --formula (cook a run)
    Sling {
        /// Bead title — what needs doing (Tier 0)
        #[arg(required_unless_present = "formula", conflicts_with = "formula")]
        title: Option<String>,
        /// Route to a specific pack agent (default: the rig's or camp's default_agent)
        #[arg(long)]
        agent: Option<String>,
        /// Rig (default: the only configured rig)
        #[arg(long)]
        rig: Option<String>,
        /// Cook <camp>/formulas/<name>.toml into a run (spec §8.2)
        #[arg(long, value_name = "NAME")]
        formula: Option<String>,
    },
    /// Send a turn to any running or exited session (the converse verb):
    /// live over campd's held stdin when possible, else `claude --resume`
    /// after its current turn
    Nudge {
        /// Session registry name (see `camp top`)
        session: String,
        /// The message to deliver
        text: String,
    },
    /// Answer a worker's permission request (control-plane §5.3): `camp watch`
    /// shows the BLOCKED row and its request id; this records and delivers the
    /// decision
    Decide {
        /// Session registry name (see `camp watch`)
        session: String,
        /// The CLI-minted permission request id (see `camp watch`)
        request_id: String,
        /// One of allow | allow_always | deny
        decision: String,
        /// The operator's reason — required for `deny` (the worker sees it)
        #[arg(long)]
        reason: Option<String>,
    },
    /// Show a bead's current state and full event history
    Show {
        /// Bead id
        bead: String,
        /// Emit the bead's state and history as one JSON object
        #[arg(long)]
        json: bool,
        /// Block until the bead reaches a closed status, then render
        #[arg(long)]
        wait: bool,
        /// With --wait, bound the wait to N seconds (default: unbounded)
        #[arg(long, value_name = "SECONDS", requires = "wait")]
        timeout: Option<u64>,
    },
    /// Ranked full-text search over everything, all time
    Search {
        /// FTS5 query (bare terms AND; "quoted phrase"; prefix*)
        query: String,
        /// Maximum number of hits
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Store a persistent memory (a memory-type bead; title = the fact)
    Remember {
        /// The fact to remember
        fact: String,
        /// Rig (default: the only configured rig)
        #[arg(long)]
        rig: Option<String>,
    },
    /// Search memories only
    Recall {
        /// FTS5 query (bare terms AND; "quoted phrase"; prefix*)
        query: String,
        /// Maximum number of hits
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Export the camp for Gas City import (spec §15.3): beads.jsonl,
    /// pinned formulas, and a pack directory. Read-only — camp never
    /// writes into a live city's store.
    Export {
        /// Output directory (created; must not already contain anything)
        #[arg(long, value_name = "DIR")]
        city: PathBuf,
        /// Skip orders that cannot be translated to gc order TOML
        /// instead of failing the export
        #[arg(long)]
        skip_untranslatable: bool,
    },
    /// Manage orders (scheduled and event-triggered formulas)
    Order {
        #[command(subcommand)]
        command: OrderCommand,
    },
    /// Manage pack imports (compat §7: the binding namespace)
    Import {
        #[command(subcommand)]
        command: ImportCommand,
    },
    /// Run the daemon in the foreground (also reachable via a campd symlink,
    /// or as `camp campd` — the two names are the same command)
    #[command(visible_alias = "campd")]
    Daemon,
    /// Stop the running daemon gracefully
    Stop,
    /// Reconcile the session registry against reality (auto at campd start)
    Adopt,
    /// Re-arm a bead whose dispatch failed, keeping its id and history
    /// (campd must be running). See `camp show <bead>` / `camp top`.
    Retry {
        /// Bead id (the one shown as dispatch-failed by `camp show`/`camp ls`)
        bead: String,
    },
    /// Register or end an attended session (the plugin's SessionStart /
    /// SessionEnd hooks wrap these; spec §8.4/§13.2)
    Session {
        #[command(subcommand)]
        command: SessionCommand,
    },
    /// List every live session by name — the overseer's one-shot snapshot of
    /// the fleet (control-plane §5.4), sourced only from the socket's
    /// `sessions.list` verb. `--json` emits the raw SessionInfo array. campd
    /// must be running (a pure client — no file reads, no pids).
    Sessions {
        /// Emit the live-session array as one JSON line (machine read).
        #[arg(long)]
        json: bool,
    },
    /// One campd status snapshot as plain text (campd must be running)
    Top {
        /// Render the compact fleet badge (▲live ●ready ✖red) from a
        /// read-only socket query. Prints nothing and notes on stderr when
        /// campd is down, exiting 0 (spec §11).
        #[arg(long)]
        statusline: bool,
    },
    /// Watch the fleet live: one line per session, push-driven from the socket
    /// (control-plane §5.1). campd must be running.
    Watch,
    /// Attach to one worker's live typed event stream (control-plane §5.2):
    /// tool calls, results, assistant text, usage -- rendered live. Replays the
    /// full history by default (a finished session ends); `--tail` follows live
    /// only; `--from <offset>` resumes from a durable byte cursor. While
    /// attached, a line is a turn, `/interrupt` stops the turn, `/q` detaches.
    /// campd must be running.
    Attach {
        /// The session NAME (from `camp watch` / `camp top`).
        session: String,
        /// Filter: all|text|tools|edits|failures (default all).
        #[arg(long, default_value = "all")]
        only: String,
        /// Follow live only -- skip the replayed history.
        #[arg(long)]
        tail: bool,
        /// Resume from a durable byte offset (a prior subscription's cursor).
        #[arg(long)]
        from: Option<u64>,
    },
    /// Interrupt a live worker's current turn (control-plane §5.4) — a one-shot
    /// over the socket's `session.interrupt` verb. The non-interactive sibling
    /// of `camp attach`'s `/interrupt`. campd must be running (a pure client —
    /// a turn is stoppable only through the pipe campd holds).
    Interrupt {
        /// The session NAME (from `camp sessions` / `camp watch`).
        session: String,
    },
    /// Operator mailbox (compat §8.2): read the mail workers send to the human.
    Mail {
        #[command(subcommand)]
        cmd: MailCommand,
    },
    /// Write a consistent, integrity-checked copy of the ledger (VACUUM
    /// INTO). DEST must not already exist.
    Backup {
        /// Destination file for the backup copy.
        dest: PathBuf,
    },
    /// Manage the camp's host service unit (launchd / systemd --user)
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
    /// gc pack worker shim (compat §6): installed into `.camp/bin`, dispatch-
    /// only, not for humans. Translates `gc <verb> …` to camp's ledger.
    #[command(hide = true)]
    GcShim {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// bd pack worker shim (compat §6): the `bd` half of the same contract.
    #[command(hide = true)]
    BdShim {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

#[derive(Subcommand)]
enum SessionCommand {
    /// Register an attended session (appends session.woke)
    Register {
        /// Session registry name (unique); derived from stdin with --hook-stdin
        #[arg(long, required_unless_present = "hook_stdin")]
        name: Option<String>,
        /// Agent role name (e.g. "attended"); derived with --hook-stdin
        #[arg(long, required_unless_present = "hook_stdin")]
        agent: Option<String>,
        /// Rig the session works in
        #[arg(long)]
        rig: Option<String>,
        /// Claude Code session id
        #[arg(long = "session-id")]
        session_id: Option<String>,
        /// Transcript file path (patrol's stall-watch target)
        #[arg(long)]
        transcript: Option<String>,
        /// OS process id
        #[arg(long)]
        pid: Option<i64>,
        /// A bead this session has claimed
        #[arg(long)]
        bead: Option<String>,
        /// Worktree the session runs in, when isolated
        #[arg(long)]
        worktree: Option<String>,
        /// Event actor / provenance (default: hook:session-start)
        #[arg(long, default_value = "hook:session-start")]
        actor: String,
        /// Read a Claude Code SessionStart hook payload from stdin; derive
        /// name=attended/<session_id>, agent=attended. Idempotent.
        #[arg(long)]
        hook_stdin: bool,
    },
    /// End an attended session (appends session.stopped)
    End {
        /// Session registry name; derived from stdin with --hook-stdin
        #[arg(long, required_unless_present = "hook_stdin")]
        name: Option<String>,
        /// How the session ended (searchable note)
        #[arg(long)]
        reason: Option<String>,
        /// Process exit code (audit-only)
        #[arg(long = "exit-code")]
        exit_code: Option<i64>,
        /// Terminating signal (audit-only)
        #[arg(long)]
        signal: Option<i64>,
        /// Event actor / provenance (default: hook:session-end)
        #[arg(long, default_value = "hook:session-end")]
        actor: String,
        /// Read a Claude Code SessionEnd hook payload from stdin; derive
        /// name=attended/<session_id>, reason=<source>.
        #[arg(long)]
        hook_stdin: bool,
        /// No-op (success) unless the session is currently live — for
        /// fire-and-forget hooks that must not error on an unknown session.
        #[arg(long)]
        if_registered: bool,
    },
}

#[derive(Subcommand)]
enum OrderCommand {
    /// List configured orders with their next fire times
    Ls {
        /// Emit JSON
        #[arg(long)]
        json: bool,
    },
    /// Fire an order now (manual trigger; campd cooks it)
    Run {
        /// Order name from camp.toml
        name: String,
    },
    /// Arm an imported order (add it to [orders] enabled)
    Enable {
        /// The imported order name (<binding>.<stem>)
        name: String,
    },
    /// Disarm an imported order (remove it from [orders] enabled)
    Disable {
        /// The imported order name (<binding>.<stem>)
        name: String,
    },
}

#[derive(Subcommand)]
enum ImportCommand {
    /// Add a pack import under a binding (clones, materializes, locks)
    Add {
        /// Pack source (path, file://, https://…, git@…)
        source: String,
        /// The binding name (agents resolve as <binding>.<agent>)
        #[arg(long)]
        name: Option<String>,
        /// A pinned ref (sha:<sha>, tag, branch) — overrides any #ref in the source
        #[arg(long)]
        version: Option<String>,
        /// Install the pack's skills/ into dispatched worktrees (§5.3). Omit for
        /// the default (install when the pack ships skills/); `--skills false`
        /// opts out even when it does.
        #[arg(long)]
        skills: Option<bool>,
    },
    /// Re-materialize every locked import (never re-resolves a ref)
    Install,
    /// Re-resolve a ref and move an import's commit
    Upgrade {
        /// Limit the upgrade to one named import
        name: Option<String>,
    },
    /// Offline: verify every locked import's materialized tree exists
    Check,
    /// List locked imports with provenance
    List,
    /// Drop an import's lock entry + materialized tree
    Remove {
        /// The binding to remove
        name: String,
    },
}

#[derive(Subcommand)]
enum MailCommand {
    /// Send mail to `human` (any other recipient is refused — gastown/v2).
    Send {
        /// Recipient (only `human` is served in v1).
        recipient: String,
        /// Positional body (joined with spaces; ignored when `-m` is given).
        body: Vec<String>,
        #[arg(short = 's', long)]
        subject: Option<String>,
        #[arg(short = 'm', long)]
        message: Option<String>,
        #[arg(long)]
        rig: Option<String>,
    },
    /// List unread mail.
    Inbox {
        #[arg(long)]
        json: bool,
    },
    /// Print a message and mark it read.
    Read { id: String },
    /// Archive (close) one or more messages.
    Archive { ids: Vec<String> },
    /// Print the unread count.
    Count,
    /// Exit 0 if unread mail exists, 1 if empty (gc's contract).
    Check,
}

#[derive(Subcommand)]
enum ServiceCommand {
    /// Install and start this camp's host service unit
    Install,
    /// Stop, unload and remove this camp's host service unit
    Uninstall,
    /// The unit's state and campd's liveness
    Status,
    /// Cycle the daemon (the post-upgrade path)
    Restart,
    /// Stop the supervised campd (the unit stays installed)
    Stop,
    /// Start a stopped but still-installed unit
    Start,
    /// Every camp with a managed unit, and its state (needs no camp)
    List,
}

#[derive(Subcommand)]
enum EventCommand {
    /// Record a worker.milestone breadcrumb
    Emit {
        /// What just happened, one line
        text: String,
        /// The bead this milestone belongs to
        #[arg(long)]
        bead: Option<String>,
        /// Emitting session name (actor attribution)
        #[arg(long)]
        session: Option<String>,
    },
}

#[derive(Subcommand)]
enum RigCommand {
    /// Register a repository as a rig
    Add {
        /// Path to the repository
        path: PathBuf,
        /// Bead id prefix (default: derived from the name; e.g. --prefix gc)
        #[arg(long)]
        prefix: Option<String>,
        /// Rig name (default: the directory's basename)
        #[arg(long)]
        name: Option<String>,
    },
    /// List configured rigs
    Ls {
        /// Emit JSON
        #[arg(long)]
        json: bool,
    },
}

/// The camp binary in daemon mode (plan decision 2: `[[bin]] camp` plus a
/// campd symlink created on install; `main` dispatches on argv[0]).
#[derive(Parser)]
#[command(
    name = "campd",
    version,
    about = "Gas Camp daemon (the camp binary in daemon mode)"
)]
struct CampdCli {
    /// Camp directory (default: $CAMP_DIR, else walk up from cwd for .camp/)
    #[arg(long, value_name = "DIR")]
    camp: Option<PathBuf>,
}

fn main() -> ExitCode {
    if invoked_as_campd() {
        let cli = CampdCli::parse();
        return report("campd", run_daemon(cli.camp.as_deref()));
    }
    let cli = Cli::parse();
    report("camp", run(cli))
}

fn invoked_as_campd() -> bool {
    std::env::args_os()
        .next()
        .is_some_and(|arg0| Path::new(&arg0).file_stem() == Some(OsStr::new("campd")))
}

fn report(name: &str, result: anyhow::Result<()>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{name}: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run_daemon(camp_flag: Option<&Path>) -> anyhow::Result<()> {
    let camp = CampDir::resolve(camp_flag)?;
    daemon::run(&camp)
}

fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Command::Init {
            service,
            no_service,
            exists_ok,
            import,
            no_import,
        } => {
            // Two bools at the CLI edge; ONE tri-state inside (clap already
            // rejected the contradictory pair).
            let choice = if service {
                ServiceChoice::Force
            } else if no_service {
                ServiceChoice::Skip
            } else {
                ServiceChoice::Auto
            };
            cmd::init::run(
                cli.camp.as_deref(),
                choice,
                exists_ok,
                import.as_deref(),
                no_import,
            )
        }
        Command::Doctor {
            orphan_runs,
            sweep_orphan_runs,
            ..
        } if orphan_runs => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            cmd::doctor::run_orphan_runs(&camp, sweep_orphan_runs)
        }
        Command::Doctor {
            refold: _,
            repair,
            formula,
            json,
            drain_reservations,
            release_orphans,
            compiled: _,
            ..
        } if drain_reservations => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            let _ = (repair, formula, json);
            cmd::doctor::run_drain_reservations(&camp, release_orphans)
        }
        Command::Doctor {
            refold: _,
            repair,
            formula,
            json,
            compiled,
            ..
        } => match formula {
            // --formula compiles a file THROUGH THE LAYERS: an imported formula's
            // `extends`, `description_file` and routes only resolve against a real
            // camp, so this needs the CampDir like every other verb.
            Some(path) => {
                let camp = CampDir::resolve(cli.camp.as_deref())?;
                cmd::doctor::run_formula(&camp, &path, json, compiled)
            }
            None => {
                let camp = CampDir::resolve(cli.camp.as_deref())?;
                cmd::doctor::run(&camp, repair)
            }
        },
        Command::Event { command } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            match command {
                EventCommand::Emit {
                    text,
                    bead,
                    session,
                } => cmd::event_emit::run(&camp, text, bead, session),
            }
        }
        Command::Events { json, from, to } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            cmd::events::run(&camp, json, from, to)
        }
        Command::Rig { command } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            match command {
                RigCommand::Add { path, prefix, name } => cmd::rig::add(&camp, path, prefix, name),
                RigCommand::Ls { json } => cmd::rig::ls(&camp, json),
            }
        }
        Command::Create {
            title,
            rig,
            description,
            needs,
            labels,
            bead_type,
            assignee,
            run,
        } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            cmd::create::run(
                &camp,
                title,
                rig,
                description,
                needs,
                labels,
                bead_type,
                assignee,
                run,
            )
        }
        Command::Claim { bead, session } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            cmd::claim::run(&camp, bead, session)
        }
        Command::Close {
            bead,
            outcome,
            reason,
            transient,
            output_json,
            work_outcome,
            work_commit,
            work_branch,
        } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            cmd::close::run(
                &camp,
                bead,
                outcome,
                reason,
                transient,
                output_json,
                work_outcome,
                work_commit,
                work_branch,
            )
        }
        Command::Ls {
            ready,
            mine,
            rig,
            json,
        } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            cmd::ls::run(&camp, ready, mine, rig, json)
        }
        Command::Sling {
            title,
            agent,
            rig,
            formula,
        } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            cmd::sling::run(&camp, title, agent, rig, formula)
        }
        Command::Nudge { session, text } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            cmd::nudge::run(&camp, session, text)
        }
        Command::Decide {
            session,
            request_id,
            decision,
            reason,
        } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            cmd::decide::run(&camp, session, request_id, decision, reason)
        }
        Command::Show {
            bead,
            json,
            wait,
            timeout,
        } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            cmd::show::run(&camp, bead, json, wait, timeout)
        }
        Command::Search { query, limit } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            cmd::search::run(&camp, &query, limit)
        }
        Command::Remember { fact, rig } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            cmd::remember::run(&camp, fact, rig)
        }
        Command::Recall { query, limit } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            cmd::recall::run(&camp, &query, limit)
        }
        Command::Export {
            city,
            skip_untranslatable,
        } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            cmd::export::run(&camp, &city, skip_untranslatable)
        }
        Command::Order { command } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            match command {
                OrderCommand::Ls { json } => cmd::order::ls(&camp, json),
                OrderCommand::Run { name } => cmd::order::run_order(&camp, &name),
                OrderCommand::Enable { name } => cmd::order::enable_order(&camp.root, &name),
                OrderCommand::Disable { name } => cmd::order::disable_order(&camp.root, &name),
            }
        }
        Command::Import { command } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            match command {
                ImportCommand::Add {
                    source,
                    name,
                    version,
                    skills,
                } => cmd::import::run_add(
                    &camp.root,
                    &source,
                    name.as_deref(),
                    version.as_deref(),
                    skills,
                ),
                ImportCommand::Install => cmd::import::run_install(&camp.root),
                ImportCommand::Upgrade { name } => {
                    cmd::import::run_upgrade(&camp.root, name.as_deref())
                }
                ImportCommand::Check => cmd::import::run_check(&camp.root),
                ImportCommand::List => cmd::import::run_list(&camp.root),
                ImportCommand::Remove { name } => cmd::import::run_remove(&camp.root, &name),
            }
        }
        Command::Daemon => run_daemon(cli.camp.as_deref()),
        Command::Adopt => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            cmd::adopt::run(&camp)
        }
        Command::Retry { bead } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            cmd::retry::run(&camp, bead)
        }
        Command::Stop => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            cmd::stop::run(&camp)
        }
        Command::Session { command } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            match command {
                SessionCommand::Register {
                    name,
                    agent,
                    rig,
                    session_id,
                    transcript,
                    pid,
                    bead,
                    worktree,
                    actor,
                    hook_stdin,
                } => cmd::session::register(
                    &camp, name, agent, rig, session_id, transcript, pid, bead, worktree, actor,
                    hook_stdin,
                ),
                SessionCommand::End {
                    name,
                    reason,
                    exit_code,
                    signal,
                    actor,
                    hook_stdin,
                    if_registered,
                } => cmd::session::end(
                    &camp,
                    name,
                    reason,
                    exit_code,
                    signal,
                    actor,
                    hook_stdin,
                    if_registered,
                ),
            }
        }
        Command::Sessions { json } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            cmd::sessions::run(&camp, json)
        }
        Command::Top { statusline } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            if statusline {
                cmd::top::statusline(&camp)
            } else {
                cmd::top::run(&camp)
            }
        }
        Command::Watch => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            cmd::watch::run(&camp)
        }
        Command::Attach {
            session,
            only,
            tail,
            from,
        } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            let filter = cmd::attach::AttachFilter::parse(&only)?;
            cmd::attach::run(&camp, session, filter, tail, from)
        }
        Command::Interrupt { session } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            cmd::interrupt::run(&camp, session)
        }
        Command::Mail { cmd } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            match cmd {
                MailCommand::Send {
                    recipient,
                    body,
                    subject,
                    message,
                    rig,
                } => {
                    let body = message.unwrap_or_else(|| body.join(" "));
                    cmd::mail::send(&camp, &recipient, subject, body, rig)
                }
                MailCommand::Inbox { json } => cmd::mail::inbox(&camp, json),
                MailCommand::Read { id } => cmd::mail::read(&camp, &id),
                MailCommand::Archive { ids } => cmd::mail::archive(&camp, &ids),
                MailCommand::Count => cmd::mail::count(&camp),
                MailCommand::Check => {
                    // BYPASS report(): an empty inbox = exit 1 is a NORMAL
                    // outcome (A2), like the shim drain — NOT an error. A count
                    // query, never a loop (invariant 1).
                    let ledger = camp_core::ledger::Ledger::open(&camp.db_path())?;
                    let n = ledger.unread_mail_count()?;
                    println!("{n}");
                    std::process::exit(if n > 0 { 0 } else { 1 });
                }
            }
        }
        Command::Backup { dest } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            cmd::backup::run(&camp, dest)
        }
        Command::Service { command } => match command {
            ServiceCommand::Install => {
                let camp = CampDir::resolve(cli.camp.as_deref())?;
                cmd::service::run_install(&camp)
            }
            ServiceCommand::Uninstall => {
                let camp = CampDir::resolve(cli.camp.as_deref())?;
                cmd::service::run_uninstall(&camp)
            }
            ServiceCommand::Status => {
                let camp = CampDir::resolve(cli.camp.as_deref())?;
                cmd::service::run_status(&camp)
            }
            ServiceCommand::Restart => {
                let camp = CampDir::resolve(cli.camp.as_deref())?;
                cmd::service::run_restart(&camp)
            }
            ServiceCommand::Stop => {
                let camp = CampDir::resolve(cli.camp.as_deref())?;
                cmd::service::run_stop(&camp)
            }
            ServiceCommand::Start => {
                let camp = CampDir::resolve(cli.camp.as_deref())?;
                cmd::service::run_start(&camp)
            }
            // `list` is the fleet view: it deliberately does NOT resolve a
            // camp — the installed units are the registry (design §5).
            ServiceCommand::List => cmd::service::run_list(),
        },
        // The two shim entry points BYPASS `report()`: a shim's outcome is a
        // process exit code `report` cannot express (drain = exit 1 is a NORMAL
        // outcome, not an error). The shim is a short-lived leaf whose ledger
        // writes are committed before return, so `std::process::exit` skipping
        // Drop is safe and deliberate (compat §6, Task 4 B8).
        Command::GcShim { args } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            match cmd::shim::gc_shim(&camp, args) {
                Ok(code) => std::process::exit(i32::from(code.0)),
                Err(error) => {
                    eprintln!("camp: {error:#}");
                    std::process::exit(1);
                }
            }
        }
        Command::BdShim { args } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            match cmd::shim::bd_shim(&camp, args) {
                Ok(code) => std::process::exit(i32::from(code.0)),
                Err(error) => {
                    eprintln!("camp: {error:#}");
                    std::process::exit(1);
                }
            }
        }
    }
}

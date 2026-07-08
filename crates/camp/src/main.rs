#![forbid(unsafe_code)]

mod campdir;
mod daemon;
mod cmd {
    pub mod adopt;
    pub mod backup;
    pub mod claim;
    pub mod close;
    pub mod create;
    pub mod doctor;
    pub mod event_emit;
    pub mod events;
    pub mod export;
    pub mod init;
    pub mod ls;
    pub mod order;
    pub mod recall;
    pub mod remember;
    pub mod rig;
    pub mod search;
    pub mod show;
    pub mod sling;
    pub mod stop;
    pub mod top;
}

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use campdir::CampDir;

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
    Init,
    /// Verify ledger invariants
    #[command(group(
        clap::ArgGroup::new("mode").required(true).args(["refold", "formula"])
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
    /// Show a bead's current state and full event history
    Show {
        /// Bead id
        bead: String,
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
    /// Run the daemon in the foreground (also reachable via a campd symlink)
    Daemon,
    /// Stop the running daemon gracefully
    Stop,
    /// Reconcile the session registry against reality (auto at campd start)
    Adopt,
    /// One campd status snapshot as plain text (auto-starts the daemon)
    Top,
    /// Write a consistent, integrity-checked copy of the ledger (VACUUM
    /// INTO). DEST must not already exist.
    Backup {
        /// Destination file for the backup copy.
        dest: PathBuf,
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
        Command::Init => cmd::init::run(cli.camp.as_deref()),
        Command::Doctor {
            refold: _,
            repair,
            formula,
        } => match formula {
            // --formula validates a file, not a camp — no CampDir needed.
            Some(path) => cmd::doctor::run_formula(&path),
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
        } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            cmd::close::run(&camp, bead, outcome, reason, transient, output_json)
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
        Command::Show { bead } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            cmd::show::run(&camp, bead)
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
            }
        }
        Command::Daemon => run_daemon(cli.camp.as_deref()),
        Command::Adopt => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            cmd::adopt::run(&camp)
        }
        Command::Stop => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            cmd::stop::run(&camp)
        }
        Command::Top => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            cmd::top::run(&camp)
        }
        Command::Backup { dest } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            cmd::backup::run(&camp, dest)
        }
    }
}

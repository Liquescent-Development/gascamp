#![forbid(unsafe_code)]

mod campdir;
mod cmd {
    pub mod claim;
    pub mod close;
    pub mod create;
    pub mod doctor;
    pub mod events;
    pub mod init;
    pub mod ls;
    pub mod recall;
    pub mod remember;
    pub mod rig;
    pub mod search;
    pub mod show;
}

use std::path::PathBuf;
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
    Doctor {
        /// Rebuild state from the event log and report drift (spec §13.5)
        #[arg(long, required = true)]
        refold: bool,
        /// Replace the state tables with the refolded content
        #[arg(long, requires = "refold")]
        repair: bool,
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

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("camp: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Command::Init => cmd::init::run(cli.camp.as_deref()),
        Command::Doctor { refold: _, repair } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            cmd::doctor::run(&camp, repair)
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
        } => {
            let camp = CampDir::resolve(cli.camp.as_deref())?;
            cmd::close::run(&camp, bead, outcome, reason)
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
    }
}

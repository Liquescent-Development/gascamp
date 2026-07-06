#![forbid(unsafe_code)]

mod campdir;
mod cmd {
    pub mod doctor;
    pub mod events;
    pub mod init;
    pub mod rig;
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
                RigCommand::Add {
                    path,
                    prefix,
                    name,
                } => cmd::rig::add(&camp, path, prefix, name),
                RigCommand::Ls { json } => cmd::rig::ls(&camp, json),
            }
        }
    }
}

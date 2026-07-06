#![forbid(unsafe_code)]

use clap::Parser;

#[derive(Parser)]
#[command(
    name = "camp",
    version,
    about = "Gas Camp: durable agent work, one SQLite ledger, zero idle cost"
)]
struct Cli {}

fn main() {
    let _cli = Cli::parse();
}

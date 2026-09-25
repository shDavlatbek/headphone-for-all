//! `hfa`: headless headphone-for-all hub / sender, discovery, trust management and selftest.
//! See `docs/CONTRACTS.md` §7.

mod cli;
mod commands;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use crate::cli::{Cli, Command};

fn init_tracing(verbose: u8) {
    let default = match verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    // Ignore the error if a subscriber is already installed.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    init_tracing(cli.verbose);
    let data_dir = cli.data_dir;
    match cli.command {
        Command::Hub(args) => commands::hub(data_dir, args).await,
        Command::Send(args) => commands::send(data_dir, args).await,
        Command::Discover(args) => commands::discover(args).await,
        Command::Devices => commands::devices().await,
        Command::Trust { command } => commands::trust(data_dir, command).await,
        Command::Selftest(args) => commands::selftest(args).await,
    }
}

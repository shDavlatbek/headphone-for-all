//! `hfa`: headless headphone-for-all hub / sender, discovery, trust management and selftest.
//! See `docs/CONTRACTS.md` §7.

mod analysis;
mod cli;
mod commands;
mod display;
mod hub;
mod onset;
mod selftest;
mod send;

use std::io::IsTerminal;
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use crate::cli::{Cli, Command};

/// How long the runtime waits for leftover blocking tasks (e.g. an mDNS browse thread
/// hand-off) after the command returned.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

/// Log filter for `-v` counts. The commands print their own user-facing output, so without
/// `-v` only warnings and errors are logged (they would scramble the live tables otherwise).
fn default_filter(verbose: u8) -> &'static str {
    match verbose {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    }
}

/// Colours in log lines only on a VT-capable terminal and without `NO_COLOR`
/// (<https://no-color.org>), so pipes, files and CI logs get plain text.
fn log_colours(stderr_is_terminal: bool, no_color: bool) -> bool {
    !no_color && display::vt_console(stderr_is_terminal)
}

fn init_tracing(verbose: u8) {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(default_filter(verbose)));
    let ansi = log_colours(
        std::io::stderr().is_terminal(),
        std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()),
    );
    // Ignore the error if a subscriber is already installed.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(ansi)
        .try_init();
}

async fn run(cli: Cli) -> anyhow::Result<()> {
    let data_dir = cli.data_dir;
    match cli.command {
        Command::Hub(args) => hub::run(commands::data_dir(data_dir)?, args).await,
        Command::Send(args) => send::run(commands::data_dir(data_dir)?, args).await,
        Command::Discover(args) => commands::discover(data_dir, args).await,
        Command::Devices => commands::devices().await,
        Command::Trust { command } => commands::trust(commands::data_dir(data_dir)?, command).await,
        Command::Selftest(args) => selftest::run(args).await,
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    init_tracing(cli.verbose);
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("hfa-rt")
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: cannot start the async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    let result = runtime.block_on(run(cli));
    runtime.shutdown_timeout(SHUTDOWN_TIMEOUT);
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{default_filter, log_colours};

    #[test]
    fn no_colours_off_a_terminal_or_with_no_color() {
        assert!(!log_colours(false, false), "pipe or file");
        assert!(!log_colours(true, true), "NO_COLOR");
    }

    #[test]
    fn verbosity_levels() {
        assert_eq!(default_filter(0), "warn");
        assert_eq!(default_filter(1), "info");
        assert_eq!(default_filter(2), "debug");
        assert_eq!(default_filter(7), "trace");
    }
}

//! `kranz` binary — thin shim over the `kranz_cli` library so integration
//! tests can drive parsing/rendering/enqueueing without spawning processes.

use clap::Parser;
use kranz_cli::cli::Cli;
use kranz_cli::commands;
use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    match commands::run_cli(cli).await {
        Ok(code) => ExitCode::from(code.clamp(0, 255) as u8),
        Err(e) => {
            eprintln!("kranz: {e:#}");
            ExitCode::from(1)
        }
    }
}

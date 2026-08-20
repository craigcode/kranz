//! `kranz` binary — thin shim over the `kranz_cli` library so integration
//! tests can drive parsing/rendering/enqueueing without spawning processes.

use clap::Parser;
use kranz_cli::cli::Cli;
use kranz_cli::commands;
use std::process::ExitCode;

#[cfg(windows)]
const WINDOWS_MAIN_STACK_BYTES: usize = 8 * 1024 * 1024;

#[cfg(windows)]
fn main() -> ExitCode {
    // The debug MSVC build can reserve a large entry frame before the first
    // statement executes (the command dispatcher carries many async states).
    // Windows' default executable stack is smaller than the explicit stacks
    // used by Tokio workers, so enter all CLI and private launcher modes on a
    // deliberately sized thread instead of failing before private argv can be
    // routed. Stack reserve is virtual; pages commit only as they are used.
    let thread = match std::thread::Builder::new()
        .name("kranz-main".to_string())
        .stack_size(WINDOWS_MAIN_STACK_BYTES)
        .spawn(main_inner)
    {
        Ok(thread) => thread,
        Err(error) => {
            eprintln!("kranz: failed to start the Windows main thread: {error}");
            return ExitCode::from(1);
        }
    };
    match thread.join() {
        Ok(code) => code,
        Err(_) => {
            eprintln!("kranz: Windows main thread panicked");
            ExitCode::from(1)
        }
    }
}

#[cfg(not(windows))]
fn main() -> ExitCode {
    main_inner()
}

fn main_inner() -> ExitCode {
    // Windows AppContainer sessions re-enter this trusted binary as a thin
    // native launcher. Route that private argv before clap, tracing, or TLS
    // initialization so the helper writes only the contained child's bytes to
    // the inherited protocol pipes.
    #[cfg(windows)]
    if kranz_engine::sandbox_windows::internal_hostile_child_requested() {
        return match kranz_engine::sandbox_windows::run_internal_hostile_child() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("kranz AppContainer hostile child: {error}");
                ExitCode::from(1)
            }
        };
    }

    #[cfg(windows)]
    if kranz_engine::sandbox_windows::internal_launcher_requested() {
        return match kranz_engine::sandbox_windows::run_internal_launcher() {
            Ok(code) => ExitCode::from(code.clamp(0, 255) as u8),
            Err(error) => {
                eprintln!("kranz AppContainer launcher: {error}");
                ExitCode::from(1)
            }
        };
    }

    #[cfg(windows)]
    if kranz_engine::sandbox_windows::internal_self_test_requested() {
        return match kranz_engine::sandbox_windows::run_production_hostile_self_test() {
            Ok(receipt) => {
                println!("{receipt}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("kranz AppContainer production self-test: {error}");
                ExitCode::from(1)
            }
        };
    }

    #[cfg(windows)]
    if kranz_engine::sandbox_windows::internal_gate_self_test_requested() {
        return match kranz_engine::sandbox_windows::run_production_gate_self_test() {
            Ok(receipt) => {
                println!("{receipt}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("kranz AppContainer production gate self-test: {error}");
                ExitCode::from(1)
            }
        };
    }

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("kranz: failed to initialize async runtime: {error}");
            return ExitCode::from(1);
        }
    };
    runtime.block_on(run_cli())
}

async fn run_cli() -> ExitCode {
    // The dep graph enables BOTH rustls crypto backends (aws-lc-rs via
    // reqwest, ring via the OTLP client), so rustls cannot auto-select and
    // panics at the first TLS connection (found live: the Slack bridge's
    // socket open killed a tokio worker). Pick one, process-wide, first.
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("install rustls ring CryptoProvider before any TLS use");

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

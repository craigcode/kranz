//! Test-only adapter: the live acceptance audit executes worker-authored code.
//! Reuse the production gate sandbox, even when the mission's workers ran off.
use kranz_engine::command_exec::{run_bounded_gate_command_sandboxed, MergeGatePolicy};
use kranz_engine::types::{SandboxConfig, SandboxEnforce};
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 {
        eprintln!("usage: acceptance_gate DELIVERABLE MISSION_DIR");
        return ExitCode::FAILURE;
    }
    let policy = MergeGatePolicy {
        sandbox: SandboxConfig {
            enforce: SandboxEnforce::Fs,
            ..SandboxConfig::default()
        },
        mission_dir: PathBuf::from(&args[1]),
    };
    let (ok, output) = run_bounded_gate_command_sandboxed(
        &PathBuf::from(&args[0]),
        "python3 acceptance_contract.py --final",
        &policy,
    );
    eprint!("{output}");
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

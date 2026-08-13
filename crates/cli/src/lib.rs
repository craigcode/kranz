#![allow(rustdoc::private_intra_doc_links)]

//! kranz CLI — clap surface, command implementations, and terminal rendering.
//!
//! The binary (`src/main.rs`) is a thin shim over this library so the
//! integration tests can drive argument parsing, status rendering, control
//! enqueueing and mission selection directly, without spawning processes
//! (and without ever spawning a real `claude` binary).
//!
//! Module map:
//! - [`cli`] — the clap derive types (`kranz <subcommand>`)
//! - [`commands`] — one function per subcommand + shared helpers
//! - [`config_cmd`] — `kranz config show|set|unset|role` (layered files +
//!   mid-mission role changes)
//! - [`exec`] — `kranz exec -f mission.md`, fully headless missions for CI
//! - [`hook_guard`] — `kranz hook-guard`, the Claude Code lifecycle-hook
//!   command the engine installs into worker sessions (internal plumbing)
//! - [`hook_status`] — `kranz hook-status`, the cursor CLI lifecycle-hook
//!   signal relay (internal plumbing, observational only)
//! - [`otel`] — event-log-to-span mapping for the `kranz otel` sidecar
//! - [`output`] — pure rendering (status tree, plan review, cost estimate)
//! - [`ready`] — repo-readiness scorecard for onboarding
//! - [`tail`] — the live event printer used by `kranz run`
//! - [`planning_tui`] — the full-screen interactive planning UI

mod embedded_dashboard {
    include!(concat!(env!("OUT_DIR"), "/embedded_dashboard.rs"));
}

pub mod amm;
pub mod backlog;
pub mod cli;
pub mod commands;
pub mod config_cmd;
pub mod exec;
pub mod hook_guard;
pub mod hook_status;
pub mod host_bridge;
pub mod init;
pub mod merged_costs;
pub mod otel;
pub mod output;
pub mod planning_tui;
pub mod ready;
pub mod tail;
pub mod ticket_notes;

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
//! - [`output`] — pure rendering (status tree, plan review, cost estimate)
//! - [`tail`] — the live event printer used by `kranz run`
//! - [`planning_tui`] — the full-screen interactive planning UI

mod embedded_dashboard {
    include!(concat!(env!("OUT_DIR"), "/embedded_dashboard.rs"));
}

pub mod cli;
pub mod commands;
pub mod output;
pub mod planning_tui;
pub mod tail;

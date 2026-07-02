//! clap derive surface of the `kranz` binary (plan §5 Phase 1-2).

use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Long help for `kranz msg` (plan §4.5): the queue/interrupt semantics must
/// be documented verbatim in `--help`.
pub const MSG_LONG_ABOUT: &str = "Queue a message for the mission's orchestrator.\n\n\
     Messages queue and are processed between worker runs by default; the \
     orchestrator reads them at its next decision point and records a \
     decision. Passing --interrupt additionally aborts the current worker \
     run (recorded as partial) before injecting the message — use it when \
     the current work is headed the wrong way.";

/// kranz — a local mission-control harness: an orchestrator plans, fresh
/// Claude Code sessions implement features, validators judge milestones,
/// and git is the source of truth.
#[derive(Parser, Debug)]
#[command(name = "kranz", version, about, author = None)]
pub struct Cli {
    /// Target repository root (defaults to the current directory)
    #[arg(long, global = true, value_name = "PATH")]
    pub repo: Option<PathBuf>,

    /// Mission id (defaults to the repo's only mission; with several, the
    /// one whose event log was updated most recently)
    #[arg(long, global = true, value_name = "ID")]
    pub mission: Option<String>,

    /// Steal a stale engine lock left behind by a crashed engine process
    #[arg(long, global = true)]
    pub force_lock: bool,

    /// DANGEROUS: bypass all permission gating for every agent session
    /// (bypassPermissions). Loud, never the default.
    #[arg(long, global = true)]
    pub dangerously_allow_all: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Create a mission and shape its plan in an interactive conversation
    Plan {
        /// The mission goal, in plain language
        goal: String,
    },

    /// Execute the mission loop (also crash-resumes an interrupted mission)
    Run,

    /// Show the mission tree, totals and recent decisions (read-only, no lock)
    Status {
        /// Dump the full MissionState as JSON instead of the tree
        #[arg(long)]
        json: bool,
    },

    /// Pause the mission (takes effect between worker runs)
    Pause,

    /// Resume a paused mission (takes effect between worker runs)
    Resume,

    /// Queue a message for the orchestrator
    #[command(long_about = MSG_LONG_ABOUT)]
    Msg {
        /// The message text
        text: String,

        /// Abort the current worker run (recorded as partial) before
        /// injecting the message
        #[arg(long)]
        interrupt: bool,
    },

    /// List this repo's missions
    Missions,

    /// Serve the REST/WebSocket API (and the dashboard, if built)
    Serve {
        /// TCP port to bind on 127.0.0.1
        #[arg(long, default_value_t = 4560)]
        port: u16,

        /// Open the dashboard in the default browser
        #[arg(long)]
        open: bool,
    },
}

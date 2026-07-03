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
    /// Create a mission and shape its plan in an interactive conversation.
    ///
    /// With no goal, resumes the most recent mission still in planning
    /// (e.g. after a Claude usage-limit interruption) — the orchestrator
    /// session is resumed with its full conversation context.
    Plan {
        /// The mission goal, in plain language (omit to resume planning)
        goal: Option<String>,
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

    /// Work with mission tickets (the backlog): list, show, new, approve
    Ticket {
        #[command(subcommand)]
        command: TicketCommand,
    },

    /// Draft a plan for a ticket non-interactively (orchestrator only).
    ///
    /// Seeds the orchestrator with the whole ticket, requests the plan, and
    /// either parks a committed plan.md for review (default) or, with --yes,
    /// approves and queues it immediately. If the orchestrator needs more
    /// context, its questions are appended to the ticket and the ticket is
    /// flagged NEEDS-CONTEXT. Spend is bounded by the orchestrator budget cap.
    Draft {
        /// The ticket slug (file stem under .kranz/tickets/)
        slug: String,

        /// Approve and enqueue the plan immediately instead of parking it for
        /// review
        #[arg(long)]
        yes: bool,
    },

    /// Show the per-repo execution queue
    Queue,

    /// Drain the execution queue: run queued missions one at a time per repo
    Work {
        /// Process exactly one front entry (exit 0 if the repo is busy)
        /// instead of draining until the queue is empty
        #[arg(long)]
        once: bool,
    },

    /// Serve the REST/WebSocket API (and the dashboard, if built)
    Serve {
        /// TCP port to bind on 127.0.0.1
        #[arg(long, default_value_t = 4560)]
        port: u16,

        /// Open the dashboard in the default browser
        #[arg(long)]
        open: bool,

        /// Directory holding the built dashboard (index.html + assets).
        /// Default search order: $KRANZ_DASHBOARD_DIST, <repo>/apps/dashboard/dist,
        /// installed asset dirs, the kranz source checkout used to build the
        /// binary, then the embedded dashboard bundled into the CLI.
        #[arg(long, value_name = "DIR")]
        dashboard: Option<std::path::PathBuf>,

        /// Pin the mutation token instead of generating one (scripting).
        /// Every POST /api/... must carry it in the x-kranz-token header.
        #[arg(long, value_name = "TOKEN")]
        token: Option<String>,
    },
}

/// Subcommands under `kranz ticket` — the backlog surface.
#[derive(Subcommand, Debug)]
pub enum TicketCommand {
    /// List tickets with slug, priority, pipeline state, and title
    List,

    /// Show one ticket: parsed fields, its state, and any needs-context block
    Show {
        /// The ticket slug (file stem under .kranz/tickets/)
        slug: String,
    },

    /// Scaffold a new ticket at .kranz/tickets/<slug>.md (refuses to overwrite)
    New {
        /// The ticket slug (used as the file stem)
        slug: String,

        /// The ticket title (frontmatter `title`)
        #[arg(long)]
        title: String,

        /// An optional one-paragraph goal to pre-fill the `## Goal` section
        #[arg(long)]
        goal: Option<String>,
    },

    /// Approve a drafted (REVIEW) ticket: enqueue its mission and mark it QUEUED
    Approve {
        /// The ticket slug
        slug: String,

        /// The drafted mission id (auto-detected from the ticket goal if omitted)
        #[arg(long, value_name = "ID")]
        mission: Option<String>,
    },
}

//! clap derive surface of the `kranz` binary (plan §5 Phase 1-2).

use clap::{Parser, Subcommand};
use kranz_engine::event_log::LockForce;
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

    /// Steal the engine lock unless its holder is provably ALIVE (a lock
    /// whose holder is provably dead is stolen automatically, without this
    /// flag; a live holder additionally needs --dangerously-steal-live-lock)
    #[arg(long, global = true)]
    pub force_lock: bool,

    /// DANGEROUS: steal the engine lock even from a provably LIVE holder
    /// (implies --force-lock). Only for a holder you have verified — e.g. via
    /// `ps -p <pid>` — to be a zombie or foreign process: stealing from a
    /// running kranz engine lets two engines corrupt one event log.
    #[arg(long, global = true, hide_short_help = true)]
    pub dangerously_steal_live_lock: bool,

    /// DANGEROUS: bypass all permission gating for every agent session
    /// (bypassPermissions). Loud, never the default.
    #[arg(long, global = true)]
    pub dangerously_allow_all: bool,

    #[command(subcommand)]
    pub command: Command,
}

impl Cli {
    /// Map the two lock flags onto the engine's [`LockForce`] tier. Passing
    /// both is fine — the strongest wins (--dangerously-steal-live-lock
    /// implies --force-lock).
    pub fn lock_force(&self) -> LockForce {
        if self.dangerously_steal_live_lock {
            LockForce::EvenIfLive
        } else if self.force_lock {
            LockForce::IfNotLive
        } else {
            LockForce::No
        }
    }
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

    /// Request a revised plan for an active mission
    Revise {
        /// Mission id to revise
        id: String,

        /// Operator instructions for the revision
        #[arg(required = true, num_args = 1.., trailing_var_arg = true)]
        instructions: Vec<String>,
    },

    /// Approve or reject a pending plan revision
    Revision {
        #[command(subcommand)]
        command: RevisionCommand,
    },

    /// List this repo's missions
    Missions,

    /// Retire a mission: mark it ABANDONED (a terminal state, not a failure).
    ///
    /// Appends `mission.abandoned` to the event log and stops here — git
    /// branches, tags, and the deliverable are left untouched. A mission that
    /// is already terminal (Complete/Failed/Abandoned) is rejected. If a live
    /// engine still holds the mission lock, stop it first; --force-lock
    /// steals only a lock whose holder is not provably alive, and
    /// --dangerously-steal-live-lock steals even a live one.
    Abandon {
        /// The mission id (defaults to the global --mission / auto-selection)
        id: Option<String>,

        /// Why the mission is being retired (recorded on the event)
        #[arg(long, value_name = "TEXT")]
        reason: Option<String>,
    },

    /// Remove stale mission directories under .kranz/missions/.
    ///
    /// Cleans Failed, Abandoned, and abandoned-in-planning husks (Planning
    /// with no plan.json) by default; --all additionally removes Complete
    /// missions. A mission whose lock is held by a live engine is never
    /// cleaned. Only mission directories are removed — git branches/tags and
    /// the missions index.md are left intact.
    Clean {
        /// Skip the confirmation prompt (assume yes)
        #[arg(long)]
        yes: bool,

        /// Also remove Complete missions (kept by default for review)
        #[arg(long)]
        all: bool,
    },

    /// Work with mission tickets (the backlog): list, show, new, queue
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

    /// Run a mission fully headlessly from a plan file (CI: plan in, exit code out).
    ///
    /// The file is a ticket-shaped markdown (`## Goal`, `## Context`, `##
    /// Scoping answers`, `## Acceptance hints`). exec seeds the orchestrator
    /// with the whole file, auto-approves the returned plan (no human), and
    /// runs the mission to a terminal state. Events stream to stderr; the only
    /// line on stdout is `kranz exec <id> <STATUS> cost=$X.XX branch=<b>`.
    ///
    /// Exit codes: 0 complete, 1 failed, 2 blocked, 3 underspecified (the
    /// orchestrator wanted clarification a headless run cannot provide — make
    /// the plan file self-sufficient and re-run). stdin is never read.
    Exec {
        /// The mission plan file (ticket-shaped markdown)
        #[arg(short = 'f', long = "file", value_name = "MISSION.md")]
        file: std::path::PathBuf,

        /// Accepted for symmetry; headless runs always auto-approve (no-op)
        #[arg(long)]
        yes: bool,

        /// Override maxFixCyclesPerMilestone for this run (bounds CI spend)
        #[arg(long, value_name = "N")]
        max_cycles: Option<u32>,

        /// After a COMPLETE run, push the mission's `kranz/*` branch to this
        /// git remote (the cloud-mission handoff: a human reviews the branch
        /// and opens the PR). Refuses to push anything but a kranz/* ref.
        #[arg(long, value_name = "REMOTE")]
        push: Option<String>,

        /// Override the unattended scrutiny floor: without this, exec refuses
        /// to run a mission whose config has skipScrutiny set, since a headless
        /// run with the scrutiny validator disabled has no adversarial reader
        /// and can pass its own tautological acceptance (see docs/gascity.md
        /// lesson 3). The `KRANZ_ALLOW_UNVALIDATED=1` env var is equivalent.
        #[arg(long)]
        allow_unvalidated: bool,
    },

    /// Show the per-repo execution queue
    Queue,

    /// Scan a git diff for unwaived secret findings.
    Scan {
        /// Scan staged changes (`git diff --cached`)
        #[arg(long)]
        staged: bool,

        /// Scan a git range such as `main..HEAD`
        #[arg(long, value_name = "A..B")]
        range: Option<String>,
    },

    /// Score how ready this repo is for autonomous kranz missions.
    Ready {
        /// Print the serializable scorecard JSON.
        #[arg(long)]
        json: bool,
    },

    /// Drain the execution queue: run queued missions one at a time per repo
    Work {
        /// Process exactly one front entry (exit 0 if the repo is busy)
        /// instead of draining until the queue is empty
        #[arg(long)]
        once: bool,
    },

    /// Serve the REST/WebSocket API (and the dashboard, if built)
    Serve {
        /// TCP port to bind
        #[arg(long, default_value_t = 4560)]
        port: u16,

        /// Bind address. Default loopback; set e.g. 0.0.0.0 (LAN) or a
        /// tailnet IP to reach the API from other devices (glasses app,
        /// phones). Non-loopback binds require `--insecure-lan` — every
        /// `/api` GET/POST/WS then requires the mutation token (header or
        /// `?token=`).
        #[arg(long, default_value = "127.0.0.1")]
        host: String,

        /// Acknowledge that a non-loopback bind exposes the API on the
        /// network. Required when `--host` is not a loopback address;
        /// off-loopback, GETs and WS upgrades require the mutation token
        /// (same as POSTs). Ignored for loopback addresses (127.0.0.0/8,
        /// ::1).
        #[arg(long)]
        insecure_lan: bool,

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

        /// Also run the Slack bridge (Socket Mode). Requires bot/app tokens +
        /// channel in ~/.kranz/config.json or KRANZ_SLACK_* env vars; a no-op
        /// with a log line when unconfigured. See docs/backlog-and-slack.md.
        #[arg(long)]
        slack: bool,
    },

    /// Free the mission's single-writer lock held by a running `kranz serve`.
    ///
    /// POSTs to a running serve's `/api/missions/:id/release` endpoint (the
    /// CLI runs in a different process and cannot reach serve's in-memory
    /// registry directly). The mission id comes from the global --mission /
    /// auto-selection, same as `kranz abandon`.
    Release {
        /// Base URL of the running `kranz serve` instance
        #[arg(long, default_value = "http://127.0.0.1:4560")]
        url: String,

        /// Mutation token printed by `kranz serve` (falls back to $KRANZ_TOKEN)
        #[arg(long, value_name = "TOKEN")]
        token: Option<String>,
    },

    /// Tail mission event logs and export OpenTelemetry spans over OTLP HTTP.
    ///
    /// Entirely read-side: polls each in-scope mission's events.jsonl (like
    /// `kranz run`'s tail and the Slack bridge), folds spans from the event
    /// timestamps, and exports one span per closed run/milestone/mission.
    /// Runs until Ctrl-C. Honors the global --repo/--mission.
    Otel {
        /// OTLP HTTP traces endpoint, e.g. http://localhost:4318/v1/traces
        #[arg(long, value_name = "URL")]
        endpoint: String,

        /// Replay each mission's full log (spans built from event
        /// timestamps) before following live. Without this, each mission's
        /// cursor is seeded at its current head — only spans whose opening
        /// AND closing events arrive during the tail are exported.
        #[arg(long)]
        from_start: bool,
    },

    /// Inspect and edit kranz configuration (files + mid-mission changes).
    ///
    /// Config resolves from three layers, later winning: compiled-in defaults
    /// <- ~/.kranz/config.json (--global) <- <repo>/.kranz/config.json (the
    /// default target). `show` prints the effective merge; `set`/`unset` edit
    /// one layer file (validated before writing, other keys preserved);
    /// `role` is the MID-MISSION path — it enqueues a config-change control
    /// command on a running mission (the CLI twin of Slack's /kranz config),
    /// while file edits only shape future missions.
    Config {
        #[command(subcommand)]
        command: crate::config_cmd::ConfigCommand,
    },
}

#[derive(Subcommand, Debug)]
pub enum RevisionCommand {
    /// Approve a proposed plan revision
    Approve {
        /// Mission id whose pending revision should be approved
        id: String,

        /// Revision number to approve
        revision: u32,
    },

    /// Reject a proposed plan revision
    Reject {
        /// Mission id whose pending revision should be rejected
        id: String,

        /// Revision number to reject
        revision: u32,
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

    /// Queue a drafted (REVIEW) ticket: enqueue its mission and mark it QUEUED
    Queue {
        /// The ticket slug
        slug: String,

        /// The drafted mission id (auto-detected from the ticket goal if omitted)
        #[arg(long, value_name = "ID")]
        mission: Option<String>,

        /// Queue despite unsatisfied `blocked-by` dependencies (a
        /// blocked-by cycle is never overridable)
        #[arg(long)]
        force: bool,
    },

    /// Deprecated alias for `ticket queue` (kept for one release; prints a
    /// deprecation note to stderr). Do not confuse with plan approval — see
    /// docs/scoping/pipeline-view.md decision D-A.
    Approve {
        /// The ticket slug
        slug: String,

        /// The drafted mission id (auto-detected from the ticket goal if omitted)
        #[arg(long, value_name = "ID")]
        mission: Option<String>,

        /// Approve despite unsatisfied `blocked-by` dependencies (a
        /// blocked-by cycle is never overridable)
        #[arg(long)]
        force: bool,
    },
}

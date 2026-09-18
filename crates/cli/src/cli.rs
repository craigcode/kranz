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
/// headless-agent sessions implement features, validators judge milestones,
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
    /// Print Kranz, Rust dependency and embedded-dashboard license notices
    Licenses,

    /// Prepare an existing Git worktree for its first Kranz mission.
    ///
    /// Scaffolds an additive runtime-ignore block, a tracked merge-gate
    /// suite, and the tickets directory. Common Rust, Node, and Python gates
    /// are detected; unfamiliar toolchains must supply --gate. Re-running is
    /// safe: existing gates are validated and never replaced.
    Init {
        /// Unconditional validation command (repeat for multiple gates).
        /// Overrides toolchain detection when creating a new gate suite.
        #[arg(long = "gate", value_name = "COMMAND")]
        gates: Vec<String>,

        /// Register this canonical root in the global multi-repo host catalog.
        #[arg(long)]
        register: bool,

        /// Host-catalog repository id (defaults to a slug of the directory).
        #[arg(long, value_name = "ID", requires = "register")]
        id: Option<String>,

        /// Friendly name shown in the dashboard project picker.
        #[arg(long, value_name = "NAME", requires = "register")]
        display_name: Option<String>,
    },

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

    /// Probe the native Windows AppContainer candidate without launching it.
    ///
    /// Loads processmodel.dll from System32 only, checks for Microsoft's
    /// experimental process-sandbox export, and records the Windows build.
    /// API presence never enables production enforcement by itself.
    SandboxProbe {
        /// Print the stable probe report as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Prepare the Windows host for AppContainer enforcement.
    ///
    /// Adds only the two persistent, non-inheriting metadata ACEs required by
    /// Windows tools on each drive root and reapplies the documented null-device
    /// descriptor that resets at boot. Run from an elevated PowerShell;
    /// ordinary Kranz launches verify both prerequisites read-only.
    SandboxPrepare {
        /// Literal local drive root to prepare (repeat for multiple drives).
        #[arg(long = "target", value_name = "DRIVE-ROOT", required = true)]
        targets: Vec<PathBuf>,
    },

    /// Show the flight-surgeon outcomes fold: autonomy ratio, grant-latency
    /// distribution, per-task-class rows, context reuse, the rubber-stamp
    /// flag, and the escalation ledger (read-only, no lock)
    Outcomes {
        /// Dump the full Outcomes struct as JSON instead of the text report
        #[arg(long)]
        json: bool,

        /// Cost per merged change grouped by repo across the host catalog
        /// (~/.kranz/config.json), beside the autonomy ratio (KRZ-329)
        #[arg(long)]
        all: bool,

        /// Window in days for the merged-change denominator (only with
        /// --all; default 30, inclusive at both ends)
        #[arg(long, default_value_t = kranz_engine::outcomes::DEFAULT_MERGED_CHANGE_WINDOW_DAYS)]
        window_days: u64,
    },

    /// Show the flight-surgeon console: autonomy ratio split by outcome,
    /// the rubber-stamp signal (park→grant p50/p90 + sub-10s count), false
    /// greens (completed missions with traced defect tickets), and the
    /// escalation ledger (read-only, no lock)
    EscalationMetrics {
        /// Dump the full EscalationMetrics struct as JSON instead of the text
        /// report
        #[arg(long)]
        json: bool,
    },

    /// Replay why a mission's unit passed from its event log alone: the gate
    /// ladder in order (verdicts + artefact resolution against the mission
    /// dir), each session's backend/model and prompt identity, every human
    /// decision with its event seq, and the terminal outcome (read-only, no
    /// lock). A cleaned runs/ degrades artefact refs to "unresolved", never
    /// to an error.
    Provenance {
        /// The mission id (defaults to the global --mission / auto-selection)
        mission_id: Option<String>,

        /// Dump the full ProvenanceChain struct as JSON instead of the text
        /// report
        #[arg(long)]
        json: bool,
    },

    /// Show the recorded evaluation series for one gate identity across all
    /// missions: every gate.result with that gate name, in log order —
    /// verdict, and the gate-supplied score + threshold where the gate
    /// reported them (read-only, no lock). kranz records what gates report,
    /// never normalizes it, and never derives the verdict from the score;
    /// a boolean-only gate's series shows verdicts with no score column
    /// (absence is the normal case, never a zero).
    GateScores {
        /// The gate identity (the `gate` field on gate.result events — a
        /// defect-class name like `vacuous-filter`, a pack gate name,
        /// `merge-gate-suite`)
        gate: String,

        /// Dump the GateScoreSeries struct as JSON instead of the text table
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

    /// Approve or deny a parked capability-grant request
    Grant {
        #[command(subcommand)]
        command: GrantCommand,
    },
    /// Inspect or answer an exact live ACP invocation (no mission-wide grant).
    Permission {
        #[command(subcommand)]
        command: PermissionCommand,
    },

    /// Answer an open structured human question (the pending-decision
    /// projection the dashboard and Slack also render)
    Question {
        #[command(subcommand)]
        command: QuestionCommand,
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

        /// Seed the ticket's `traced-from-mission` frontmatter with this
        /// mission id (drafting a defect ticket traced back to the mission
        /// that shipped the defect — the flight-surgeon false-green join)
        #[arg(long = "from-mission", value_name = "MISSION_ID")]
        from_mission: Option<String>,
    },

    /// Decompose a complex goal into a ticket DAG (blocked-by edges).
    ///
    /// One planner turn proposes 1..=8 tickets as JSON; the proposed DAG
    /// (slugs, titles, priorities, edges) is printed for review. Without
    /// --yes nothing is written (dry-run preview). With --yes all tickets are
    /// written at once: slug rules, unknown blockers, a missing root, or a
    /// blocked-by cycle each refuse the whole write loudly — no partial
    /// writes. Every emitted ticket is an ordinary ticket: draft it with
    /// `kranz draft <slug>`, queue it with `kranz ticket queue <slug>`; deps
    /// gating keeps a node from running before its blockers Complete.
    Decompose {
        /// The complex goal, in plain language
        goal: String,

        /// Write the proposed tickets (without this flag it is a dry-run preview)
        #[arg(long, short = 'y')]
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

        /// Create, plan, approve, and enqueue the mission without running it.
        /// A later `kranz work` drain owns execution. This is the native-queue
        /// handoff for headless producers such as the Gas City pack.
        #[arg(long, conflicts_with = "push")]
        enqueue: bool,

        /// Stable producer name recorded beside an enqueued mission so its
        /// terminal state can be returned even if another dispatcher drains
        /// the shared queue. Must be paired with --enqueue-external-ref.
        #[arg(
            long,
            value_name = "PRODUCER",
            requires_all = ["enqueue", "enqueue_external_ref"]
        )]
        enqueue_source: Option<String>,

        /// Producer-owned identifier recorded with --enqueue-source.
        #[arg(
            long,
            value_name = "REF",
            requires_all = ["enqueue", "enqueue_source"]
        )]
        enqueue_external_ref: Option<String>,

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

    /// Show or remove an entry from the per-repo execution queue
    Queue {
        /// Remove the queued entry for this mission without running it. The
        /// mission itself is retained for audit; abandon it separately when
        /// the producer is cancelling the work rather than re-enqueueing it.
        #[arg(long, value_name = "MISSION_ID")]
        remove: Option<String>,
    },

    /// Report docs/knowledge notes whose verified_against paths drifted.
    KnowledgeRefresh {
        /// Print the report as JSON instead of text
        #[arg(long)]
        json: bool,
    },

    /// Scan a git diff for unwaived secret findings.
    Scan {
        /// Scan staged changes (`git diff --cached`)
        #[arg(long)]
        staged: bool,

        /// Scan a git range such as `main..HEAD`
        #[arg(long, value_name = "A..B")]
        range: Option<String>,
    },

    /// Lint the scoped tree for banned domain vocabulary (the KRZ-314
    /// clean-room boundary: kranz core stays domain-free, domain knowledge
    /// ships in private packs). Policy is the committed hashed denylist
    /// (.kranz/domain-denylist.json) plus reviewed waivers
    /// (.kranz/domain-allowlist); see docs/domain-lint.md. Exit 0 clean, 1
    /// on unwaived hits — each named by fingerprint + file:line, never
    /// quoting the matched term.
    DomainLint {
        /// Regenerate the hashed denylist from a plaintext terms file (one
        /// term per line, `#` comments) instead of linting. The terms file
        /// IS the protected vocabulary: keep it out of the repo —
        /// .kranz/domain-terms.local is gitignored for exactly this.
        #[arg(long, value_name = "TERMS_FILE")]
        seed_config: Option<PathBuf>,

        /// Print the report as JSON instead of text
        #[arg(long)]
        json: bool,
    },

    /// INTERNAL: the Claude Code lifecycle-hook command the engine installs
    /// into worker sessions (KRZ-302). Never invoked by operators — the
    /// session's CLI pipes a PreToolUse hook payload to stdin; the guard
    /// judges it against the engine-written spec file, records the outcome,
    /// and exits 0 (allow) / 2 (block, stderr fed to the model) / 1 (guard
    /// error, failing open — the engine-side sweep remains authoritative).
    HookGuard {
        /// The per-session hook-gate spec file the engine wrote
        #[arg(long, value_name = "PATH")]
        config: PathBuf,
    },

    /// INTERNAL: the cursor CLI lifecycle-hook relay the backend installs
    /// into agent sessions (ticket `agent-hooks-status-signals`). Never
    /// invoked by operators — the session's CLI pipes a lifecycle hook
    /// payload to stdin; the relay maps it to a coarse signal and POSTs it
    /// to the loopback endpoint in the engine-written spec file. Purely
    /// observational: every failure exits 0.
    HookStatus {
        /// The per-session hook-status spec file the engine wrote
        #[arg(long, value_name = "PATH")]
        config: PathBuf,
    },

    /// Score how ready this repo is for autonomous kranz missions.
    Ready {
        /// Print the serializable scorecard JSON.
        #[arg(long)]
        json: bool,
        /// Score every repo in the host catalog (~/.kranz/config.json) and
        /// report the N-of-M-at-L3+ org headline.
        #[arg(long)]
        all: bool,
    },

    /// Drain the execution queue: run queued missions one at a time per repo
    Work {
        /// Process exactly one front entry (exit 0 if the repo is busy)
        /// instead of draining until the queue is empty
        #[arg(long)]
        once: bool,

        /// With --once, run only when this exact mission is still at the
        /// front. A changed front is released without execution.
        #[arg(long, value_name = "MISSION_ID", requires = "once")]
        expect: Option<String>,
    },

    /// Serve the REST/WebSocket API (and the dashboard, if built)
    Serve {
        /// TCP port to bind
        #[arg(long, default_value_t = 4560)]
        port: u16,

        /// Bind address. Default loopback; set e.g. 0.0.0.0 (LAN) or a
        /// tailnet IP to reach the API from other devices (glasses app,
        /// phones). Non-loopback binds require `--insecure-lan` — every
        /// `/api` GET/POST/WS then requires a token. Reads accept the
        /// read-only token; POSTs require the mutation token.
        #[arg(long, default_value = "127.0.0.1")]
        host: String,

        /// Acknowledge that a non-loopback bind exposes the API on the
        /// network. Required when `--host` is not a loopback address;
        /// off-loopback, GETs and WS upgrades require either the read-only or
        /// mutation token; POSTs require the mutation token. Ignored for
        /// loopback addresses (127.0.0.0/8, ::1).
        #[arg(long)]
        insecure_lan: bool,

        /// Require either the read-only or mutation token on `/api` GETs and
        /// the WS upgrade on ANY bind class, including loopback. POSTs still
        /// require the mutation token. Off-loopback binds already gate reads;
        /// this flag forces that posture on loopback too. Still requires
        /// `--insecure-lan` for a non-loopback bind (unchanged).
        #[arg(long)]
        read_auth: bool,

        /// Open the dashboard in the default browser
        #[arg(long)]
        open: bool,

        /// Directory holding the built dashboard (index.html + assets).
        /// Default search order: `$KRANZ_DASHBOARD_DIST`, `<repo>/apps/dashboard/dist`,
        /// installed asset dirs, the kranz source checkout used to build the
        /// binary, then the embedded dashboard bundled into the CLI.
        #[arg(long, value_name = "DIR")]
        dashboard: Option<std::path::PathBuf>,

        /// Pin the mutation token instead of generating one (scripting).
        /// Every POST /api/... must carry it in the x-kranz-token header.
        /// Falls back to $KRANZ_TOKEN when unset.
        #[arg(long, value_name = "TOKEN")]
        token: Option<String>,

        /// Pin the read-only token instead of generating one (falls back to
        /// $KRANZ_READ_TOKEN). Authenticates /api GETs and the WS upgrade
        /// only — never mutations — so it is the token safe to hand to
        /// dashboards and agents. Stored next to serve.token at
        /// .kranz/serve.read.token (operator catalog:
        /// `~/.kranz/serve/<endpoint>.read.token`). Must be non-empty visible
        /// ASCII without whitespace and differ from the mutation token.
        #[arg(long, value_name = "TOKEN")]
        read_token: Option<String>,

        /// Also run the Slack bridge (Socket Mode). Requires bot/app tokens +
        /// channel in ~/.kranz/config.json or KRANZ_SLACK_* env vars; a no-op
        /// with a log line when unconfigured. See docs/backlog-and-slack.md.
        #[arg(long)]
        slack: bool,
    },

    /// Free the mission's single-writer lock held by a running `kranz serve`.
    ///
    /// Resolves the selected root against the running serve's live catalog,
    /// then POSTs to its repository-scoped release endpoint. The CLI runs in
    /// a different process and cannot reach serve's in-memory registry
    /// directly. The mission id comes from the global --mission /
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

    /// Export a mission's portable audit bundle (KRZ-326): a self-contained
    /// directory an auditor can open without repo access — manifest.json
    /// (every entry with its sha256 + source ref), a human summary.md, the
    /// provenance chain.json, the escalation ledger and cost fold, the raw
    /// scrubbed event log, and every resolvable artefact's bytes under
    /// artefacts/. Missing artefact bytes are listed as unresolved manifest
    /// entries, never omitted. The same log always yields the same bundle.
    EvidenceBundle {
        /// The mission id (defaults to the global --mission / auto-selection)
        mission_id: Option<String>,

        /// Directory to write the bundle into (created; must be empty).
        /// Defaults to `./evidence-bundle-<mission-id>`
        #[arg(long, value_name = "DIR")]
        out: Option<PathBuf>,
    },

    /// Export validation-PASSED worker traces as fine-tuning-ready JSONL.
    ///
    /// Derived and regenerable: loads and folds the target mission's event
    /// log on demand (like `status`) and prints one instruction-pair JSON
    /// object per line to stdout — there is no persisted dataset file, so
    /// re-running this command over an unchanged event log always yields
    /// byte-identical output.
    ExportTraces {
        /// The mission id (defaults to the global --mission / auto-selection;
        /// ignored with --all)
        mission_id: Option<String>,

        /// Aggregate passed traces across every mission under
        /// .kranz/missions. A mission whose event log is missing or
        /// unreadable is skipped, not fatal.
        #[arg(long)]
        all: bool,

        /// Write the JSONL output to this path instead of stdout.
        #[arg(long, value_name = "PATH")]
        out: Option<PathBuf>,
    },

    /// Export the provenance-tagged training corpus as JSONL (KRZ-332).
    ///
    /// One tagged record per line (`source`: worker-trace / divergence /
    /// escalation): validation-PASSED worker traces, divergence
    /// comparison+resolution pairs, and escalation-ledger human judgments —
    /// every record carrying the provenance refs (mission, backend/model,
    /// run id, gate-chain seqs) that resolve it through `kranz provenance`.
    /// Derived and regenerable like export-traces (which stays a
    /// traces-only contract): same logs in, byte-identical JSONL out.
    ExportCorpus {
        /// The mission id (defaults to the global --mission / auto-selection;
        /// ignored with --all)
        mission_id: Option<String>,

        /// Aggregate the corpus across every mission under .kranz/missions
        /// (ids sorted). A mission whose event log is missing, unreadable,
        /// or corrupt is skipped, not fatal.
        #[arg(long)]
        all: bool,

        /// Write the JSONL output to this path instead of stdout.
        #[arg(long, value_name = "PATH")]
        out: Option<PathBuf>,
    },

    /// Inspect and edit kranz configuration (files + mid-mission changes).
    ///
    /// Config resolves from three layers, later winning: compiled-in defaults
    /// <- `~/.kranz/config.json` (`--global`) <- `<repo>/.kranz/config.json` (the
    /// default target). `show` prints the effective merge; `set`/`unset` edit
    /// one layer file (validated before writing, other keys preserved);
    /// `role` is the MID-MISSION path — it enqueues a config-change control
    /// command on a running mission (the CLI twin of Slack's /kranz config),
    /// while file edits only shape future missions.
    Config {
        #[command(subcommand)]
        command: crate::config_cmd::ConfigCommand,
    },

    /// Work with kranz packs (the pack contract: deterministic gates, role
    /// prompts, checklists, artefact stores — docs/pack-contract.md)
    Pack {
        #[command(subcommand)]
        command: PackCommand,
    },

    /// Work with Flight Rules standards (KRZ-341): the schema-4 pack
    /// standards corpus — RFCs, rules, the normalized manifest + content
    /// digest, and the lifecycle transition lint
    /// (docs/scoping/flight-rules-engineering-standards.md)
    Standards {
        #[command(subcommand)]
        command: StandardsCommand,
    },
}

/// Subcommands under `kranz pack` — the pack contract surface (ticket
/// `.kranz/tickets/pack-contract-gates-prompts.md`).
#[derive(Subcommand, Debug)]
pub enum PackCommand {
    /// Load and validate a pack directory fully locally, printing what it
    /// registers (gates, prompts, checklists, artefact stores).
    ///
    /// A directory without a pack.toml is not a pack — the command says so
    /// plainly and exits 0. An invalid pack fails closed: nonzero exit
    /// naming the offending field (unknown field, wrong type, missing
    /// required key, empty gate command, duplicate name, model-judged gate
    /// kind, engine-reserved gate name).
    Lint {
        /// The pack directory containing pack.toml
        dir: PathBuf,
    },
}

/// Subcommands under `kranz standards` — the Flight Rules surface (ticket
/// `.kranz/tickets/flight-rules-pack-contract.md`, KRZ-341).
#[derive(Subcommand, Debug)]
pub enum StandardsCommand {
    /// Fold Flight Rules effectiveness across mission event logs and traced
    /// defect tickets. Raw denominators are always shown; interpretive smells
    /// remain suppressed below the documented minimum sample count.
    Metrics {
        /// Emit the deterministic machine-readable report
        #[arg(long)]
        json: bool,
    },

    /// Load a pack's `[standards]` corpus and print the normalized manifest:
    /// every RFC and rule with its effective lifecycle status, checker
    /// binding, and scopes, plus the sha256 content digest and the trust
    /// posture (an external/untracked pack is advisory-only — enforced rules
    /// are refused at load naming the remedy).
    ///
    /// With `--against <ref>`, the base pack is read from TRACKED BLOBS at
    /// that git ref (never the worktree) and lifecycle transition violations
    /// are refused: absent/draft → enforced, a semantic rule change without
    /// a revision increment, a disappeared known rule ID, tombstone
    /// reactivation. Exit 0 clean, 1 on load errors or refused transitions.
    Lint {
        /// The pack directory containing pack.toml
        dir: PathBuf,

        /// Base git ref (branch or sha) whose tracked pack bytes define the
        /// approved lifecycle state for the transition check
        #[arg(long, value_name = "REF")]
        against: Option<String>,
    },

    /// Record an authorized human waiver for ONE standards failure (ticket
    /// flight-rules-waiver-decisions, KRZ-344; design D-I) — the only
    /// approval surface. Displays the finding, the pinned rule, the
    /// affected paths, and the diff digest the waiver binds, then appends
    /// `standards.waiver.approved` to the mission log. Refuses: a rule with
    /// `waivable: false`, a rule absent from the approved pin (an expired/
    /// retired rule or RFC is never pinned), a mismatched revision, an
    /// absent finding, an already-waived finding, or a past expiry. The
    /// approver is recorded honestly as `local-operator` plus this surface
    /// — a model may request a waiver but can never approve one.
    Waive {
        /// The pinned rule id to except (e.g. ENG-RUST-014)
        #[arg(long)]
        rule: String,

        /// The revision you believe you are waiving (defaults to the pinned
        /// revision; a mismatch refuses rather than silently rebinding)
        #[arg(long)]
        revision: Option<u64>,

        /// Waive only the latest finding with this subject (disambiguates
        /// when several findings cite the rule)
        #[arg(long)]
        finding: Option<String>,

        /// Why the exception is granted (recorded verbatim)
        #[arg(long)]
        reason: String,

        /// Expiry instant, RFC 3339 (e.g. 2026-09-01T00:00:00Z) — must be
        /// in the future; waivers are never permanent
        #[arg(long, value_name = "RFC3339")]
        expires: String,
    },

    /// Record the authorized human verdict for one approval-pinned
    /// `manual-attestation` rule. The attestation binds to the current
    /// affected paths and diff digest, so any relevant change invalidates
    /// it. The approver is always the local operator using this CLI surface.
    Attest {
        /// The pinned manual-attestation rule id
        #[arg(long)]
        rule: String,

        /// Why the operator judges the current change compliant
        #[arg(long)]
        reason: String,
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

#[derive(Subcommand, Debug)]
pub enum GrantCommand {
    /// Approve the parked grant request (extend command_grants + re-validate)
    Approve {
        /// Mission id whose pending grant should be approved
        id: String,

        /// The exact command to grant, quoted (must match the parked request)
        command: String,
    },

    /// Deny the parked grant request (block the milestone, fail closed)
    Deny {
        /// Mission id whose pending grant should be denied
        id: String,

        /// The exact command being denied, quoted (must match the parked request)
        command: String,

        /// Reason recorded on the denial
        #[arg(long, default_value = "denied by operator")]
        reason: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum PermissionCommand {
    /// Show pending requests, their complete action, binding and deadline.
    List { id: String },
    /// Allow exactly the invocation whose binding was inspected.
    Allow {
        id: String,
        request_id: String,
        #[arg(long)]
        binding: String,
    },
    /// Refuse exactly the invocation whose binding was inspected.
    Deny {
        id: String,
        request_id: String,
        #[arg(long)]
        binding: String,
    },
}

/// Subcommands under `kranz question` — the structured human-question
/// pending-decision projection (ticket structured-human-question-events).
#[derive(Subcommand, Debug)]
pub enum QuestionCommand {
    /// List the mission's open questions (id, text, options)
    List {
        /// Mission id whose open questions should be listed
        id: String,
    },

    /// Answer an open question (lands as question.answered; the answer
    /// reaches the running mission via the user-message consult)
    Answer {
        /// Mission id whose open question should be answered
        id: String,

        /// The engine-minted question id (`q-<n>`, from `kranz question list`)
        question_id: String,

        /// The answer: an offered option's text verbatim, or free text
        answer: String,

        /// 0-based index of the offered option picked (omit for free text)
        #[arg(long)]
        option: Option<u32>,
    },
}

/// Subcommands under `kranz ticket` — the backlog surface.
#[derive(Subcommand, Debug)]
pub enum TicketCommand {
    /// List tickets with slug, priority, pipeline state, and title
    List,

    /// List tickets ready to pick up now: actionable states whose
    /// `defer-until` (if any) has passed — deferred tickets stay hidden until
    /// their time (D-BW-3; the clock decides at listing time, no scheduler)
    Ready {
        /// Also list the not-yet-ready deferred tickets, with their defer
        /// times (operator visibility; the default listing stays clean)
        #[arg(long)]
        include_deferred: bool,
    },

    /// Show one ticket: parsed fields, its state, and any needs-context block
    Show {
        /// The ticket slug (file stem under .kranz/tickets/)
        slug: String,
    },

    /// Scaffold a new ticket at `.kranz/tickets/<slug>.md` (refuses to overwrite)
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

    /// Import an OpenSpec change folder (`openspec/changes/<name>/`) as a
    /// ticket. Carries the proposal and its requirements; deliberately drops
    /// `tasks.md`, and never passes SHALL scenarios off as acceptance
    /// criteria. One way only — the approved plan stays authoritative
    ImportOpenspec {
        /// Path to the OpenSpec change directory (must hold proposal.md)
        path: PathBuf,

        /// Ticket slug (defaults to the change directory's name)
        #[arg(long)]
        slug: Option<String>,
    },

    /// Append a note to a ticket's discussion
    /// (`.kranz/tickets/<slug>.notes.jsonl` — append-only, committed with the
    /// ticket; D-BW-3). Author is $KRANZ_NOTE_AUTHOR, else "operator"
    Note {
        /// The ticket slug
        slug: String,

        /// The note text (multiple words are joined with spaces)
        #[arg(required = true)]
        text: Vec<String>,
    },

    /// Print a ticket's discussion notes chronologically (append-only — there
    /// is no edit or delete, mirroring the event log's honesty posture)
    Notes {
        /// The ticket slug
        slug: String,
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

    /// Fold terminal `.status` sidecar states into committed frontmatter
    /// `state:` keys — the one-time migration from the ticket-state-
    /// frontmatter design, so done verdicts survive a fresh clone. Dry-run
    /// by default; tickets with uncommitted .md edits are skipped by name
    /// (never rewrite a file an in-flight editor or agent has open)
    MigrateState {
        /// Apply the fold (without this flag it only reports what it would do)
        #[arg(long)]
        yes: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serve_parses_read_auth_flag() {
        let cli = Cli::try_parse_from(["kranz", "serve", "--read-auth"]).unwrap();
        match cli.command {
            Command::Serve { read_auth, .. } => assert!(read_auth),
            other => panic!("expected Serve, got {other:?}"),
        }
    }

    #[test]
    fn serve_parses_read_token_flag() {
        let cli = Cli::try_parse_from(["kranz", "serve", "--read-token", "ro-123"]).unwrap();
        match cli.command {
            Command::Serve { read_token, .. } => {
                assert_eq!(read_token.as_deref(), Some("ro-123"))
            }
            other => panic!("expected Serve, got {other:?}"),
        }
    }

    #[test]
    fn pack_contract_pack_lint_parses_dir() {
        let cli = Cli::try_parse_from(["kranz", "pack", "lint", "some/dir"]).unwrap();
        match cli.command {
            Command::Pack { command } => match command {
                PackCommand::Lint { dir } => assert_eq!(dir, PathBuf::from("some/dir")),
            },
            other => panic!("expected Pack, got {other:?}"),
        }
    }

    #[test]
    fn flight_rules_contract_standards_lint_parses_dir_and_against() {
        let cli = Cli::try_parse_from(["kranz", "standards", "lint", "some/dir"]).unwrap();
        match cli.command {
            Command::Standards { command } => match command {
                StandardsCommand::Lint { dir, against } => {
                    assert_eq!(dir, PathBuf::from("some/dir"));
                    assert_eq!(against, None);
                }
                other => panic!("expected Lint, got {other:?}"),
            },
            other => panic!("expected Standards, got {other:?}"),
        }
        let cli = Cli::try_parse_from([
            "kranz",
            "standards",
            "lint",
            "some/dir",
            "--against",
            "main",
        ])
        .unwrap();
        match cli.command {
            Command::Standards { command } => match command {
                StandardsCommand::Lint { dir, against } => {
                    assert_eq!(dir, PathBuf::from("some/dir"));
                    assert_eq!(against.as_deref(), Some("main"));
                }
                other => panic!("expected Lint, got {other:?}"),
            },
            other => panic!("expected Standards, got {other:?}"),
        }
    }

    /// KRZ-344 (D-I): the waiver surface parses its full flag set; the
    /// approver is never a flag — the record honestly names
    /// `local-operator` plus the `cli` surface.
    #[test]
    fn flight_rules_waiver_standards_waive_parses_flags() {
        let cli = Cli::try_parse_from([
            "kranz",
            "standards",
            "waive",
            "--rule",
            "ZZ-FAIL-001",
            "--reason",
            "accepted risk",
            "--expires",
            "2026-09-01T00:00:00Z",
        ])
        .unwrap();
        match cli.command {
            Command::Standards { command } => match command {
                StandardsCommand::Waive {
                    rule,
                    revision,
                    finding,
                    reason,
                    expires,
                } => {
                    assert_eq!(rule, "ZZ-FAIL-001");
                    assert_eq!(revision, None);
                    assert_eq!(finding, None);
                    assert_eq!(reason, "accepted risk");
                    assert_eq!(expires, "2026-09-01T00:00:00Z");
                }
                other => panic!("expected Waive, got {other:?}"),
            },
            other => panic!("expected Standards, got {other:?}"),
        }
        let cli = Cli::try_parse_from([
            "kranz",
            "standards",
            "waive",
            "--rule",
            "ZZ-FAIL-001",
            "--revision",
            "2",
            "--finding",
            "a-1",
            "--reason",
            "accepted risk",
            "--expires",
            "2026-09-01T00:00:00Z",
        ])
        .unwrap();
        match cli.command {
            Command::Standards { command } => match command {
                StandardsCommand::Waive {
                    revision, finding, ..
                } => {
                    assert_eq!(revision, Some(2));
                    assert_eq!(finding.as_deref(), Some("a-1"));
                }
                other => panic!("expected Waive, got {other:?}"),
            },
            other => panic!("expected Standards, got {other:?}"),
        }
        // --reason and --expires are required: no silent permanent or
        // reason-less waiver exists.
        assert!(
            Cli::try_parse_from(["kranz", "standards", "waive", "--rule", "ZZ-FAIL-001"]).is_err()
        );
    }

    #[test]
    fn flight_rules_enforcement_standards_attest_parses_flags() {
        let cli = Cli::try_parse_from([
            "kranz",
            "standards",
            "attest",
            "--rule",
            "ZZ-MANUAL-001",
            "--reason",
            "reviewed the deployment evidence",
        ])
        .unwrap();
        match cli.command {
            Command::Standards { command } => match command {
                StandardsCommand::Attest { rule, reason } => {
                    assert_eq!(rule, "ZZ-MANUAL-001");
                    assert_eq!(reason, "reviewed the deployment evidence");
                }
                other => panic!("expected Attest, got {other:?}"),
            },
            other => panic!("expected Standards, got {other:?}"),
        }
        assert!(
            Cli::try_parse_from(["kranz", "standards", "attest", "--rule", "ZZ-MANUAL-001"])
                .is_err()
        );
    }

    #[test]
    fn flight_rules_metrics_standards_metrics_parses_json() {
        let cli = Cli::try_parse_from(["kranz", "standards", "metrics", "--json"]).unwrap();
        match cli.command {
            Command::Standards {
                command: StandardsCommand::Metrics { json },
            } => assert!(json),
            other => panic!("expected standards metrics, got {other:?}"),
        }
    }

    #[test]
    fn evidence_bundle_parses_mission_and_out() {
        let cli =
            Cli::try_parse_from(["kranz", "evidence-bundle", "m-1", "--out", "some/dir"]).unwrap();
        match cli.command {
            Command::EvidenceBundle { mission_id, out } => {
                assert_eq!(mission_id.as_deref(), Some("m-1"));
                assert_eq!(out.as_deref(), Some(PathBuf::from("some/dir").as_path()));
            }
            other => panic!("expected EvidenceBundle, got {other:?}"),
        }

        // Both optional: the mission falls back to auto-selection, the output
        // dir to ./evidence-bundle-<mission-id>.
        let cli = Cli::try_parse_from(["kranz", "evidence-bundle"]).unwrap();
        match cli.command {
            Command::EvidenceBundle { mission_id, out } => {
                assert_eq!(mission_id, None);
                assert_eq!(out, None);
            }
            other => panic!("expected EvidenceBundle, got {other:?}"),
        }
    }

    #[test]
    fn decompose_parses_goal_and_yes_flag() {
        let cli = Cli::try_parse_from(["kranz", "decompose", "build the thing", "--yes"]).unwrap();
        match cli.command {
            Command::Decompose { goal, yes } => {
                assert_eq!(goal, "build the thing");
                assert!(yes);
            }
            other => panic!("expected Decompose, got {other:?}"),
        }

        let cli = Cli::try_parse_from(["kranz", "decompose", "g", "-y"]).unwrap();
        match cli.command {
            Command::Decompose { yes, .. } => assert!(yes),
            other => panic!("expected Decompose, got {other:?}"),
        }

        // Dry-run is the default: no flag, no write.
        let cli = Cli::try_parse_from(["kranz", "decompose", "g"]).unwrap();
        match cli.command {
            Command::Decompose { yes, .. } => assert!(!yes),
            other => panic!("expected Decompose, got {other:?}"),
        }
    }

    #[test]
    fn knowledge_refresh_parses_json_flag() {
        let cli = Cli::try_parse_from(["kranz", "knowledge-refresh"]).unwrap();
        match cli.command {
            Command::KnowledgeRefresh { json } => assert!(!json),
            other => panic!("expected KnowledgeRefresh, got {other:?}"),
        }
        let cli = Cli::try_parse_from(["kranz", "knowledge-refresh", "--json"]).unwrap();
        match cli.command {
            Command::KnowledgeRefresh { json } => assert!(json),
            other => panic!("expected KnowledgeRefresh, got {other:?}"),
        }
    }

    #[test]
    fn domain_lint_command_parses_seed_config_and_json_flags() {
        // Bare form: lint mode, text output.
        let cli = Cli::try_parse_from(["kranz", "domain-lint"]).unwrap();
        match cli.command {
            Command::DomainLint { seed_config, json } => {
                assert_eq!(seed_config, None);
                assert!(!json);
            }
            other => panic!("expected DomainLint, got {other:?}"),
        }

        let cli = Cli::try_parse_from(["kranz", "domain-lint", "--seed-config", "terms", "--json"])
            .unwrap();
        match cli.command {
            Command::DomainLint { seed_config, json } => {
                assert_eq!(seed_config, Some(PathBuf::from("terms")));
                assert!(json);
            }
            other => panic!("expected DomainLint, got {other:?}"),
        }
    }

    /// Composition audit (ticket `config-fail-open-audit`): every CLI flag
    /// whose name signals a guard-weakening override must either carry the
    /// `dangerously-` prefix or be one of the enumerated, justified
    /// exceptions. A future flag that short-circuits a guard without the
    /// prefix trips this test until its justification is recorded — the
    /// naming rule's tripwire. The per-flag rationales live in
    /// docs/config-composition.md.
    #[test]
    fn composition_audit_guard_weakening_flags_are_dangerously_prefixed_or_enumerated() {
        use clap::CommandFactory;

        fn collect_long_flags(cmd: &clap::Command, out: &mut Vec<String>) {
            for arg in cmd.get_arguments() {
                if let Some(long) = arg.get_long() {
                    out.push(long.to_string());
                }
            }
            for sub in cmd.get_subcommands() {
                collect_long_flags(sub, out);
            }
        }

        let mut flags = Vec::new();
        collect_long_flags(&Cli::command(), &mut flags);
        // The heuristic: names that read like they weaken a guard. Wide on
        // purpose — a false positive only costs a recorded justification.
        let suspicious = [
            "force",
            "steal",
            "bypass",
            "unvalidated",
            "insecure",
            "skip",
            "unsafe",
            "dangerous",
            "override",
        ];
        let mut hits: Vec<String> = flags
            .into_iter()
            .filter(|flag| suspicious.iter().any(|s| flag.contains(s)))
            .collect();
        hits.sort();
        hits.dedup();

        // The documented set. `dangerously-*` members are the naming rule's
        // escape valve; the rest are the accepted exceptions of
        // docs/config-composition.md:
        // - force-lock: steals only from a holder PROVABLY dead (the
        //   liveness probe is fail-closed); the live-holder bypass is the
        //   dangerously-named flag.
        // - allow-unvalidated (exec): lifts only the unattended scrutiny
        //   FLOOR — a refuse-to-run gate, not a deny list; self-describing.
        // - insecure-lan (serve): an acknowledgment that ADDS token
        //   requirements on non-loopback binds; it removes nothing.
        // - force (ticket queue/approve): skips blocked-by READINESS only;
        //   dependency cycles are never overridable.
        let expected = [
            "allow-unvalidated",
            "dangerously-allow-all",
            "dangerously-steal-live-lock",
            "force",
            "force-lock",
            "insecure-lan",
        ];
        assert_eq!(
            hits,
            expected.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            "a guard-weakening flag changed: every bypass of a deny list must carry \
             the dangerously- prefix or a recorded exception (docs/config-composition.md)"
        );
    }
}

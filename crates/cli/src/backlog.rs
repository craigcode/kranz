//! The backlog CLI: `kranz ticket …`, `kranz draft`, `kranz queue`, and the
//! `kranz work` dispatcher (design: docs/backlog-and-slack.md).
//!
//! Tickets are missions-in-waiting authored as `.kranz/tickets/<slug>.md`.
//! The pipeline is: `ticket new` scaffolds one; `draft` runs the planning
//! conversation non-interactively (orchestrator only, budget-capped) and parks
//! a committed `plan.md` for review (or bounces the ticket back with the
//! orchestrator's questions); `ticket approve` enqueues the parked mission;
//! `work` drains the per-repo queue, one mission at a time.
//!
//! The rendering and decision logic here are pure functions (data → String,
//! or state → next-action) so they are unit-tested without a backend; the two
//! handlers that spawn `claude` (`draft`, `work`) are thin async wrappers over
//! the shared engine + `run_mission_loop`.

use crate::commands::{build_backend, load_config, run_mission_loop};
use crate::output;
use anyhow::{bail, Context, Result};
use kranz_engine::deps;
use kranz_engine::draft::{drive_draft, DraftOutcome};
use kranz_engine::git_ops::GitRepo;
use kranz_engine::orchestrator::MissionEngine;
use kranz_engine::queue::{self, QueueEntry};
use kranz_engine::ticket::{Ticket, TicketState};
use std::path::{Path, PathBuf};

// Relocated into kranz-engine (roadmap f-1-1): the pure draft decision helpers
// live in `kranz_engine::draft` now so any surface can reuse the sequencing
// core. Re-exported here so existing CLI callers/tests keep working.
pub use kranz_engine::draft::{draft_decision, split_questions, DraftDecision};

// Relocated into kranz-engine (roadmap f-1-1): the drain/claim/skip loop and
// its pure decision helpers live in `kranz_engine::work` now so any surface
// (CLI, REST, Slack) can drain a repo's queue. Re-exported here so existing
// CLI callers/tests keep working.
pub use kranz_engine::work::{
    next_work_action, ticket_state_for_mission, work_skip_for_failed_blocker, WorkAction,
};

// ---------------------------------------------------------------------------
// ticket new — scaffold
// ---------------------------------------------------------------------------

/// Back-compat wrapper (no context) over [`Ticket::ticket_template`], which
/// now lives in `kranz-engine` so the REST `POST /api/tickets` handler shares
/// it instead of duplicating.
pub fn ticket_template(title: &str, goal: Option<&str>) -> String {
    Ticket::ticket_template(title, goal, None)
}

/// Scaffold `.kranz/tickets/<slug>.md`. Refuses (error) if a ticket with that
/// slug already exists. Thin wrapper over [`Ticket::scaffold`]. Returns the
/// written path.
pub fn cmd_ticket_new(repo: &Path, slug: &str, title: &str, goal: Option<&str>) -> Result<PathBuf> {
    Ticket::scaffold(repo, slug, title, goal, None).map_err(anyhow::Error::from)
}

// ---------------------------------------------------------------------------
// ticket list / show — pure rendering
// ---------------------------------------------------------------------------

/// UPPERCASE label for a ticket pipeline state (mirrors mission status labels).
pub fn ticket_state_label(state: TicketState) -> &'static str {
    match state {
        TicketState::New => "NEW",
        TicketState::Drafting => "DRAFTING",
        TicketState::NeedsContext => "NEEDS-CONTEXT",
        TicketState::WrongPlan => "WRONG-PLAN",
        TicketState::Review => "REVIEW",
        TicketState::Queued => "QUEUED",
        TicketState::Running => "RUNNING",
        TicketState::Done => "DONE",
        TicketState::Failed => "FAILED",
        TicketState::Parked => "PARKED",
        // Operator-closed lifecycle states (committed `state:` frontmatter):
        // rendered distinctly from DONE — no delivery happened.
        TicketState::Superseded => "SUPERSEDED",
        TicketState::Wontfix => "WONTFIX",
    }
}

/// A ticket's terminal-state label, splitting `Done` into `DELIVERED`
/// (mission complete but its branch is not yet merged into base) vs
/// `LANDED` (mission branch merged, or no mission-merge information to
/// distinguish otherwise) — reusing the engine's merged-ancestor probe
/// ([`kranz_engine::merged::ticket_merged`]) so this can never drift from the
/// REST `/api/tickets` projection. Non-`Done` states render exactly as
/// [`ticket_state_label`].
pub fn ticket_terminal_label(repo: &Path, slug: &str, state: TicketState) -> &'static str {
    if state != TicketState::Done {
        return ticket_state_label(state);
    }
    match kranz_engine::merged::ticket_merged(repo, slug) {
        Some(false) => "DELIVERED",
        Some(true) | None => "LANDED",
    }
}

/// One row of the `kranz ticket list` table: the ticket plus its resolved
/// terminal label (already split into DELIVERED/LANDED for a Done ticket).
pub struct TicketRow<'a> {
    pub ticket: &'a Ticket,
    pub label: &'static str,
}

/// Render the `kranz ticket list` table: slug, priority, state, title.
pub fn render_ticket_list(rows: &[TicketRow<'_>]) -> String {
    if rows.is_empty() {
        return "no tickets\n".to_string();
    }
    let slug_w = rows
        .iter()
        .map(|r| r.ticket.slug.len())
        .max()
        .unwrap_or(4)
        .max(4);
    let state_w = rows.iter().map(|r| r.label.len()).max().unwrap_or(5).max(5);
    let mut out = String::new();
    out.push_str(&format!(
        "{:<slug_w$}  {:<3}  {:<state_w$}  {}\n",
        "SLUG", "PRI", "STATE", "TITLE",
    ));
    for row in rows {
        out.push_str(&format!(
            "{:<slug_w$}  {:<3}  {:<state_w$}  {}\n",
            row.ticket.slug,
            row.ticket.priority,
            row.label,
            output::one_line(&row.ticket.title, 60),
        ));
    }
    out
}

/// Render the `kranz ticket ready` table: the ready rows in the same shape as
/// [`render_ticket_list`], then — only when `include_deferred` asked for it —
/// a second table of the not-yet-ready deferred tickets with their defer
/// times (D-BW-3: operator visibility without polluting the default listing).
pub fn render_ticket_ready(
    ready: &[TicketRow<'_>],
    deferred: &[TicketRow<'_>],
    include_deferred: bool,
) -> String {
    let mut out = String::new();
    if ready.is_empty() {
        out.push_str("no ready tickets\n");
    } else {
        out.push_str(&render_ticket_list(ready));
    }
    if include_deferred && !deferred.is_empty() {
        let slug_w = deferred
            .iter()
            .map(|r| r.ticket.slug.len())
            .max()
            .unwrap_or(4)
            .max(4);
        let state_w = deferred
            .iter()
            .map(|r| r.label.len())
            .max()
            .unwrap_or(5)
            .max(5);
        out.push_str("\ndeferred (not ready yet):\n");
        out.push_str(&format!(
            "{:<slug_w$}  {:<3}  {:<state_w$}  {:<25}  {}\n",
            "SLUG", "PRI", "STATE", "DEFER-UNTIL", "TITLE",
        ));
        for row in deferred {
            let until = row
                .ticket
                .defer_until
                .map(|ts| ts.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
                .unwrap_or_default();
            out.push_str(&format!(
                "{:<slug_w$}  {:<3}  {:<state_w$}  {:<25}  {}\n",
                row.ticket.slug,
                row.ticket.priority,
                row.label,
                until,
                output::one_line(&row.ticket.title, 60),
            ));
        }
    }
    out
}

/// Render `kranz ticket show <slug>`: the parsed ticket, its resolved
/// terminal label, and any "needs context" / "wrong plan" block appended to
/// the ticket body.
pub fn render_ticket_show(ticket: &Ticket, label: &str) -> String {
    // Ticket prose is unauthenticated tree data and the "needs context" /
    // "wrong plan" blocks are planner-authored, so every interpolated field
    // goes through the control-character filter (H9).
    let clean = output::sanitize_untrusted;
    let mut out = String::new();
    out.push_str(&format!("ticket {} [{}]\n", clean(&ticket.slug), label));
    out.push_str(&format!("  title:    {}\n", clean(&ticket.title)));
    out.push_str(&format!("  priority: {}\n", ticket.priority));
    out.push_str(&format!("  schedule: {:?}\n", ticket.schedule));
    if let Some(budget) = ticket.max_budget_usd {
        out.push_str(&format!("  budget:   ${budget:.2}\n"));
    }
    if !ticket.repo_refs.is_empty() {
        out.push_str(&format!(
            "  refs:     {}\n",
            clean(&ticket.repo_refs.join(", "))
        ));
    }
    if !ticket.blocked_by.is_empty() {
        out.push_str(&format!(
            "  blocked-by: {}\n",
            clean(&ticket.blocked_by.join(", "))
        ));
    }

    if !ticket.goal.trim().is_empty() {
        out.push_str("\n## Goal\n");
        out.push_str(clean(ticket.goal.trim()).trim());
        out.push('\n');
    }
    if !ticket.context.trim().is_empty() {
        out.push_str("\n## Context\n");
        out.push_str(clean(ticket.context.trim()).trim());
        out.push('\n');
    }
    if !ticket.scoping_answers.is_empty() {
        out.push_str("\n## Scoping answers\n");
        for item in &ticket.scoping_answers {
            out.push_str(&format!("- {}\n", clean(item)));
        }
    }
    if !ticket.acceptance_hints.is_empty() {
        out.push_str("\n## Acceptance hints\n");
        for item in &ticket.acceptance_hints {
            out.push_str(&format!("- {}\n", clean(item)));
        }
    }

    // The "needs context" questions are appended to the raw body by the engine;
    // surface them verbatim so `show` is enough to answer the ticket.
    if let Some(block) = section_block(&ticket.raw_body, "needs context") {
        let block = clean(&block);
        out.push('\n');
        out.push_str(&block);
        if !block.ends_with('\n') {
            out.push('\n');
        }
    }
    // Same for a draft-stage wrong-plan escalation: the planner's reason,
    // verbatim, so `show` is enough to reframe or re-scope the ticket.
    if let Some(block) = section_block(&ticket.raw_body, "wrong plan") {
        let block = clean(&block);
        out.push('\n');
        out.push_str(&block);
        if !block.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

/// Extract a `## <heading>` section (heading + following lines) from a ticket
/// body, matched case-insensitively by heading prefix (`"needs context"`,
/// `"wrong plan"`). Returns everything from that heading to the next `##`
/// heading (or end of body).
fn section_block(body: &str, heading_prefix: &str) -> Option<String> {
    let mut lines = body.lines().peekable();
    let mut collecting = false;
    let mut out: Vec<&str> = Vec::new();
    for line in &mut lines {
        let is_section = line.trim_start().starts_with("##");
        if collecting && is_section {
            break; // next section ends the block
        }
        if is_section
            && line
                .trim_start_matches('#')
                .trim()
                .to_ascii_lowercase()
                .starts_with(heading_prefix)
        {
            collecting = true;
        }
        if collecting {
            out.push(line);
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out.join("\n").trim_end().to_string())
    }
}

// ---------------------------------------------------------------------------
// queue — pure rendering
// ---------------------------------------------------------------------------

/// Render the `kranz queue` table: position, priority, mission id, ticket
/// slug, plus a header noting whether the repo is currently busy.
pub fn render_queue(entries: &[QueueEntry], busy_with: Option<&str>) -> String {
    let mut out = String::new();
    match busy_with {
        Some(id) => out.push_str(&format!("repo busy: mission {id} is running\n")),
        None => out.push_str("repo idle\n"),
    }
    if entries.is_empty() {
        out.push_str("queue empty\n");
        return out;
    }
    out.push_str(&format!(
        "{:<3}  {:<3}  {:<14}  {}\n",
        "#", "PRI", "MISSION", "TICKET"
    ));
    for (i, entry) in entries.iter().enumerate() {
        out.push_str(&format!(
            "{:<3}  {:<3}  {:<14}  {}\n",
            i + 1,
            entry.priority,
            entry.mission_id,
            entry.ticket_slug.as_deref().unwrap_or("-"),
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// Handlers (list/show/new/approve/queue are backend-free; draft/work spawn)
// ---------------------------------------------------------------------------

/// `kranz ticket list`.
pub fn cmd_ticket_list(repo: &Path) -> String {
    let tickets = Ticket::list(repo);
    let rows: Vec<TicketRow<'_>> = tickets
        .iter()
        .map(|t| {
            let state = Ticket::read_state(repo, &t.slug);
            TicketRow {
                ticket: t,
                label: ticket_terminal_label(repo, &t.slug, state),
            }
        })
        .collect();
    render_ticket_list(&rows)
}

/// `kranz ticket ready [--include-deferred]`: the pick-up-now listing
/// (D-BW-3). Ready = an actionable pipeline state (not in flight, not
/// terminal) AND no `defer-until` still in the future — a deferred ticket
/// simply becomes listable on its day; the clock at listing time is the only
/// arbiter, there is no scheduler. `--include-deferred` appends the parked
/// deferred tickets with their defer times for operator visibility.
pub fn cmd_ticket_ready(repo: &Path, include_deferred: bool) -> String {
    let now = chrono::Utc::now();
    let tickets = Ticket::list(repo);
    let mut ready: Vec<TicketRow<'_>> = Vec::new();
    let mut deferred: Vec<TicketRow<'_>> = Vec::new();
    for t in &tickets {
        let state = Ticket::read_state(repo, &t.slug);
        if !matches!(
            state,
            TicketState::New
                | TicketState::NeedsContext
                | TicketState::WrongPlan
                | TicketState::Review
                | TicketState::Parked
        ) {
            continue;
        }
        let row = TicketRow {
            ticket: t,
            label: ticket_terminal_label(repo, &t.slug, state),
        };
        if t.is_ready_at(now) {
            ready.push(row);
        } else {
            deferred.push(row);
        }
    }
    render_ticket_ready(&ready, &deferred, include_deferred)
}

/// `kranz ticket show <slug>`.
pub fn cmd_ticket_show(repo: &Path, slug: &str) -> Result<String> {
    let path = Ticket::tickets_dir(repo).join(format!("{slug}.md"));
    if !path.is_file() {
        bail!("ticket '{slug}' not found at {}", path.display());
    }
    let ticket = Ticket::load(&path)?;
    let state = Ticket::read_state(repo, slug);
    let label = ticket_terminal_label(repo, slug, state);
    Ok(render_ticket_show(&ticket, label))
}

/// `kranz queue`.
pub fn cmd_queue(repo: &Path) -> String {
    let entries = queue::list(repo);
    let busy = queue::is_repo_busy(repo);
    render_queue(&entries, busy.as_deref())
}

/// `kranz queue --remove <mission-id>`: retire only the runnable queue entry.
/// The mission record remains append-only/auditable and may be abandoned or
/// re-enqueued by the caller's own lifecycle policy.
pub fn cmd_queue_remove(repo: &Path, mission_id: &str) -> Result<String> {
    if !queue::remove(repo, mission_id) {
        bail!("mission '{mission_id}' is not queued");
    }
    Ok(format!("removed {mission_id} from the queue\n"))
}

/// Load a ticket by slug (error if missing).
fn load_ticket(repo: &Path, slug: &str) -> Result<Ticket> {
    let path = Ticket::tickets_dir(repo).join(format!("{slug}.md"));
    if !path.is_file() {
        bail!("ticket '{slug}' not found at {}", path.display());
    }
    Ok(Ticket::load(&path)?)
}

/// Apply a ticket's per-ticket budget override to the orchestrator role, so
/// draft spend is bounded by the ticket's `maxBudgetUsd` when it sets one.
fn config_for_ticket(
    mut cfg: kranz_engine::types::MissionConfig,
    ticket: &Ticket,
) -> kranz_engine::types::MissionConfig {
    if let Some(budget) = ticket.max_budget_usd {
        cfg.orchestrator.max_budget_usd = Some(budget);
    }
    cfg
}

/// `kranz draft <slug> [--yes] [--from-mission m-xxxx]`: non-interactive plan
/// drafting.
///
/// Sets the ticket Drafting, creates a mission seeded with the whole ticket,
/// requests the plan, and resolves via [`draft_decision`]:
/// - Ready → `approve_plan` (commits plan.md on the mission branch), then set
///   Review (parked) or, with `--yes`, enqueue + set Queued.
/// - NotReady → append the orchestrator's questions to the ticket and set
///   NeedsContext.
/// - WrongPlan → append the planner's escalation reason to the ticket and set
///   WrongPlan (parked for the operator; never queued).
///
/// `--from-mission` first seeds the ticket's `traced-from-mission`
/// frontmatter (drafting a defect ticket traced back to the mission that
/// shipped it — the flight-surgeon false-green join).
///
/// Only the orchestrator runs (no workers); spend is bounded by the
/// orchestrator budget cap (per-ticket override applied).
pub async fn cmd_draft(
    repo: PathBuf,
    slug: &str,
    yes: bool,
    from_mission: Option<&str>,
    dangerously_allow_all: bool,
) -> Result<i32> {
    if let Some(mission_id) = from_mission {
        Ticket::seed_traced_from_mission(&repo, slug, mission_id)
            .with_context(|| format!("seeding traced-from-mission on ticket '{slug}'"))?;
        println!("ticket '{slug}' traced from mission {mission_id}");
    }
    let ticket = load_ticket(&repo, slug)?;
    let cfg = config_for_ticket(load_config(&repo, dangerously_allow_all)?, &ticket);
    let backend = build_backend(&cfg)?;

    // Remember where the operator was: parking the plan checks out the
    // mission branch, and the draft must put the checkout back afterward.
    let original_branch = GitRepo::open(&repo)
        .ok()
        .and_then(|g| g.current_branch().ok());
    let mut engine = MissionEngine::create(backend, repo.clone(), &ticket.mission_goal(), cfg)?;
    println!(
        "drafting ticket '{slug}' as mission {}",
        engine.mission_id()
    );

    let drive = drive_draft(&mut engine, &repo, &ticket, yes)
        .await
        .map_err(|e| crate::commands::augment_limit_hint(e.into()))
        .with_context(|| format!("drafting ticket '{slug}'"))?;

    // Surface any captured seed reply (session start) for visibility, same as
    // pre-hoist `cmd_draft`.
    if let Some(seed) = &drive.seed_reply {
        println!("orchestrator: {}", output::one_line(seed, 200));
    }

    let mission_branch = engine.state().mission.mission_branch.clone();
    // The checkout only ever moves in the Approve path (`approve_plan` checks
    // out the mission branch to commit plan.md); NeedsContext/WrongPlan never
    // touch it, so — matching pre-hoist `cmd_draft` — only restore when a plan
    // was produced. Drop the engine (flush + release the mission lock) first.
    if let Some(plan) = &drive.plan {
        println!("{}", output::render_plan(plan));
        drop(engine);
        restore_draft_checkout(&repo, original_branch.as_deref(), &mission_branch);
    } else {
        drop(engine);
    }

    match drive.outcome {
        DraftOutcome::NeedsContext {
            mission_id: _,
            questions,
        } => {
            println!("ticket '{slug}' needs context — the orchestrator asked:");
            for q in &questions {
                println!("  - {}", output::sanitize_untrusted(q));
            }
            println!(
                "answer them in {} then run `kranz draft {slug}` again.",
                Ticket::tickets_dir(&repo)
                    .join(format!("{slug}.md"))
                    .display()
            );
        }
        DraftOutcome::WrongPlan { mission_id, reason } => {
            println!(
                "ticket '{slug}' WRONG-PLAN escalation (mission {mission_id}) — the planner \
                 can produce a plan but believes it is likely wrong:"
            );
            println!("  {}", output::sanitize_untrusted(&reason));
            println!(
                "edit or re-scope {} then run `kranz draft {slug}` again.",
                Ticket::tickets_dir(&repo)
                    .join(format!("{slug}.md"))
                    .display()
            );
        }
        DraftOutcome::PlanAsProse { mission_id } => {
            println!(
                "ticket '{slug}' NOT queued: mission {mission_id}'s orchestrator produced a \
                 plan but emitted it as prose instead of through the plan channel, so nothing \
                 was queued. Run `kranz draft {slug}` again."
            );
        }
        DraftOutcome::Enqueued { mission_id } => {
            println!(
                "plan committed on {mission_branch}; mission {mission_id} approved and QUEUED. \
                 Run it with `kranz work`."
            );
        }
        DraftOutcome::ParkedForReview {
            mission_id,
            mission_branch: _,
        } => {
            println!(
                "plan committed on {mission_branch} for review; mission {mission_id} parked. \
                 Review it, then run `kranz ticket approve {slug}` to queue it \
                 (or `kranz plan --mission {mission_id}` to reshape)."
            );
        }
    }
    Ok(0)
}

/// `kranz decompose <goal> [--yes]`: one planner turn decomposes a complex
/// goal into a blocked-by ticket DAG (design: .kranz/tickets/
/// ticket-dag-decomposition.md). The proposed DAG is always printed; tickets
/// are written only with `--yes` (dry-run preview otherwise), all-or-none —
/// any validation refusal (slug rules, unknown blocker, missing root, cycle)
/// writes nothing.
///
/// The sequencing core (planner turn, validation, staged write) lives in
/// [`kranz_engine::decompose`]; this wrapper keeps the CLI-only concerns —
/// config/backend resolution (the same call path as [`cmd_draft`]) and
/// printing. Emitted tickets flow through the normal draft/queue pipeline.
pub async fn cmd_decompose(
    repo: PathBuf,
    goal: &str,
    yes: bool,
    dangerously_allow_all: bool,
) -> Result<i32> {
    let cfg = load_config(&repo, dangerously_allow_all)?;
    let backend = build_backend(&cfg)?;
    let drive =
        kranz_engine::decompose::drive_decompose(backend.as_ref(), &repo, goal, &cfg, yes).await?;

    print!("{}", kranz_engine::decompose::render_preview(&drive.nodes));
    match &drive.written {
        None => println!(
            "dry run — nothing written; re-run with --yes to write these {} ticket(s).",
            drive.nodes.len()
        ),
        Some(paths) => {
            for path in paths {
                println!("wrote {}", path.display());
            }
            println!(
                "draft each node with `kranz draft <slug>` — deps gating runs the DAG in \
                 dependency order."
            );
        }
    }
    Ok(0)
}

/// `kranz ticket queue <slug> [--mission <id>]`: enqueue the parked (Review)
/// mission for the ticket and set the ticket Queued.
///
/// `draft` (no `--yes`) leaves the ticket in Review with a committed plan.md on
/// a mission branch but nothing in the queue. Queueing picks that mission:
/// the explicit `--mission` if given, else the newest mission on the repo
/// whose recorded goal equals the ticket's folded [`Ticket::mission_goal`]
/// (that is exactly what `draft` seeded it with).
///
/// The gate (cycle detection, unsatisfied-blocker refusal) and the enqueue
/// side effects live in [`deps::approve_ticket`] — the same core the REST
/// `POST /api/tickets/:slug/approve` handler calls, so the two surfaces can
/// never drift on what "approvable" means.
///
/// Ticket-queueing is verb "Queue" (see docs/scoping/pipeline-view.md
/// decision D-A); "Approve" is reserved for plan approval.
pub fn cmd_ticket_queue(
    repo: &Path,
    slug: &str,
    explicit_mission: Option<&str>,
    force: bool,
) -> Result<i32> {
    let approved = deps::approve_ticket(repo, slug, explicit_mission, force)?;
    println!(
        "ticket '{slug}' QUEUED (mission {}, priority {}). Run it with `kranz work`.",
        approved.mission_id, approved.priority
    );
    Ok(0)
}

/// Deprecated alias for [`cmd_ticket_queue`]. `kranz ticket approve` used to
/// be the only spelling for ticket-queueing; D-A renamed it to `queue` and
/// reserved "approve" for plan approval. Kept one release for compatibility.
pub fn cmd_ticket_approve(
    repo: &Path,
    slug: &str,
    explicit_mission: Option<&str>,
    force: bool,
) -> Result<i32> {
    eprintln!("warning: `kranz ticket approve` is deprecated, use `kranz ticket queue` instead");
    cmd_ticket_queue(repo, slug, explicit_mission, force)
}

/// `kranz ticket migrate-state [--yes]`: the one-time fold of terminal
/// `.status` sidecars into committed frontmatter `state:` keys (design
/// ticket-state-frontmatter, rule 4). Dry-run by default — the report lists
/// every fold it WOULD make plus the loud per-name skips — `--yes` applies.
/// The fold logic and its skip rules live in
/// [`kranz_engine::migrate_state`]; this wrapper only renders.
pub fn cmd_ticket_migrate_state(repo: &Path, yes: bool) -> Result<i32> {
    let report = kranz_engine::migrate_state::fold_sidecar_states(repo, yes)?;
    print!("{}", render_migration_report(&report));
    Ok(0)
}

/// Render the fold report: one line per fold (or would-fold), one loud line
/// per dirty skip, a line per ticket with a NON-terminal sidecar (pipeline
/// state is left sidecar-owned by design), and a summary. No-sidecar tickets
/// are summary-counted only — a line each would drown the signal.
pub fn render_migration_report(report: &kranz_engine::migrate_state::MigrationReport) -> String {
    use kranz_engine::migrate_state::FoldAction;
    let verb = if report.applied {
        "folded"
    } else {
        "would fold"
    };
    let mut out = String::new();
    for action in &report.actions {
        match action {
            FoldAction::Fold { slug, note } => {
                out.push_str(&format!(
                    "{verb} {slug}: .status done → frontmatter state: done"
                ));
                if let Some(note) = note {
                    out.push_str(&format!(" (state-note: {})", output::one_line(note, 60)));
                }
                out.push('\n');
            }
            FoldAction::SkipDirty { slug } => {
                out.push_str(&format!(
                    "SKIP {slug}: uncommitted changes — in-flight work; commit it, then re-run to fold\n"
                ));
            }
            FoldAction::AlreadyMigrated { slug } => {
                out.push_str(&format!(
                    "skip {slug}: frontmatter already carries a state: key\n"
                ));
            }
            FoldAction::NoTerminalSidecar {
                slug,
                sidecar: Some(state),
            } => {
                out.push_str(&format!(
                    "leave {slug}: sidecar state {} is pipeline, not operator lifecycle\n",
                    ticket_state_label(*state)
                ));
            }
            FoldAction::NoTerminalSidecar { sidecar: None, .. } => {}
        }
    }
    out.push_str(&format!(
        "{}: {} {}, {} dirty-skipped, {} already migrated, {} left alone (no terminal sidecar)\n",
        if report.applied { "applied" } else { "dry run" },
        report.folds(),
        if report.applied { "folded" } else { "to fold" },
        report.dirty_skips(),
        report.already_migrated(),
        report.left_alone(),
    ));
    if !report.applied && report.folds() > 0 {
        out.push_str("nothing written — re-run with --yes to apply the fold.\n");
    }
    out
}

/// Find the mission `draft` created for a ticket: the newest-by-event-log
/// mission whose recorded goal equals the ticket's folded mission goal.
/// Dispatcher-exit twin of [`restore_draft_checkout`]: put the checkout back
/// where the operator started `kranz work`. Skipped when the operator was
/// already on a mission branch (restoring TO one would recreate the very
/// stranding this exists to end).
fn restore_work_checkout(repo: &Path, original: Option<&str>) {
    let Some(original) = original else { return };
    if original.starts_with("kranz/mission-") {
        return;
    }
    let Ok(git) = GitRepo::open(repo) else { return };
    if git.current_branch().ok().as_deref() == Some(original) {
        return;
    }
    match git.is_clean_tracked() {
        Ok(true) => match git.checkout(original) {
            Ok(()) => println!("checkout restored to {original}"),
            Err(e) => eprintln!("warning: could not restore checkout to {original}: {e}"),
        },
        Ok(false) => {
            eprintln!("warning: checkout left in place: tracked files have uncommitted changes")
        }
        Err(e) => {
            eprintln!("warning: could not probe the working tree ({e}); checkout left in place")
        }
    }
}

/// Put the checkout back where the operator had it before `kranz draft`
/// parked the plan. Untracked files ride along; TRACKED modifications abort
/// the restore — never carry uncommitted operator edits across a branch
/// switch silently.
fn restore_draft_checkout(repo: &Path, original: Option<&str>, mission_branch: &str) {
    let Some(original) = original else { return };
    if original == mission_branch {
        return;
    }
    let Ok(git) = GitRepo::open(repo) else { return };
    match git.is_clean_tracked() {
        Ok(true) => match git.checkout(original) {
            Ok(()) => println!("checkout restored to {original}"),
            Err(e) => eprintln!("warning: could not restore checkout to {original}: {e}"),
        },
        Ok(false) => eprintln!(
            "warning: leaving checkout on {mission_branch}: tracked files have \
             uncommitted changes"
        ),
        Err(e) => eprintln!(
            "warning: could not probe the working tree ({e}); checkout left on {mission_branch}"
        ),
    }
}

/// `kranz work [--once]`: the dispatcher. Drains the per-repo queue one
/// mission at a time; `--once` processes exactly one front entry (or exits if
/// the repo is busy). Per-repo serialization is enforced by `is_repo_busy`.
///
/// Thin wrapper over [`kranz_engine::work::drain_queue`] (roadmap f-1-1): the
/// core loop lives in the engine so any surface can drive it headlessly; this
/// wrapper keeps the CLI-only concerns — operator checkout capture/restore,
/// live progress printing, and tailing each mission's events to stderr via
/// [`run_mission_loop`].
pub async fn cmd_work(repo: PathBuf, once: bool, expected: Option<String>) -> Result<i32> {
    // Remember the operator's checkout: each mission's run() asserts its own
    // branch, so when the dispatcher exits it puts the checkout back where
    // the operator started (tracked-dirty trees abort the restore).
    let dispatch_branch = GitRepo::open(&repo)
        .ok()
        .and_then(|g| g.current_branch().ok());
    // Claims abandoned by a crashed dispatcher come back first (review P1).
    // Reported here (rather than inside the engine core) so the CLI keeps its
    // pre-hoist wording; the core's own `recover_dead_claims` call is a no-op
    // second pass over whatever's left.
    let recovered = queue::recover_dead_claims(&repo);
    if recovered > 0 {
        println!(
            "recovered {recovered} claimed queue entr{} from dead dispatchers",
            if recovered == 1 { "y" } else { "ies" }
        );
    }

    let report =
        kranz_engine::work::drain_queue_expected(&repo, once, expected.as_deref(), |mission_id| {
            let repo = repo.clone();
            async move {
                println!("running mission {mission_id} from the queue");
                let status = drive_mission(repo, &mission_id).await;
                if let Err(e) = &status {
                    eprintln!("kranz: mission {mission_id} errored: {e:#}");
                }
                status
            }
        })
        .await?;

    if report.stopped_busy {
        // `--once` against a busy repo: nothing was claimed or run, so the
        // checkout is left exactly where the busy sibling dispatcher needs
        // it — restoring here would switch branches out from under its
        // still-running mission.
        return Ok(0);
    }
    if let Some(front) = report.expected_mismatch {
        println!(
            "queue front changed to {front}; expected {} — nothing ran",
            expected.as_deref().unwrap_or("-")
        );
        restore_work_checkout(&repo, dispatch_branch.as_deref());
        return Ok(0);
    }
    if report.ran.is_empty() && report.skipped.is_empty() && report.parked.is_empty() {
        println!("queue empty — nothing to do.");
    } else {
        if !report.parked.is_empty() {
            println!("parked (backend not ready): {}", report.parked.join(", "));
        }
        if !report.skipped.is_empty() {
            println!("skipped: {}", report.skipped.join(", "));
        }
        if !report.ran.is_empty() {
            println!("ran: {}", report.ran.join(", "));
        }
    }
    restore_work_checkout(&repo, dispatch_branch.as_deref());
    Ok(0)
}

/// Run one queued mission to a terminal state, returning the `run_mission_loop`
/// exit code (0 complete / 2 blocked / 1 failed). Extracted so `cmd_work` maps
/// it to a ticket state.
async fn drive_mission(repo: PathBuf, mission_id: &str) -> Result<i32> {
    run_mission_loop(
        repo,
        mission_id.to_string(),
        kranz_engine::event_log::LockForce::No,
        false,
    )
    .await
}

// ---------------------------------------------------------------------------
// tests — Delivered/Landed split on the CLI ticket list/show renderer
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use kranz_engine::events::{Event, EventKind};
    use std::sync::Once;
    use tempfile::TempDir;

    static ENV_ISOLATION: Once = Once::new();

    /// Mask the host's global/system git config (mirrors
    /// `crates/engine/tests/merged_test.rs::isolate_git_env`) so identity,
    /// signing, and hooks never leak into the throwaway repos.
    fn isolate_git_env() {
        ENV_ISOLATION.call_once(|| {
            let missing = std::env::temp_dir().join(format!(
                "kranz-cli-backlog-test-no-config-{}",
                std::process::id()
            ));
            std::env::set_var("GIT_CONFIG_GLOBAL", &missing);
            std::env::set_var("GIT_CONFIG_SYSTEM", &missing);
            if let Ok(ceiling) = std::fs::canonicalize(std::env::temp_dir()) {
                std::env::set_var("GIT_CEILING_DIRECTORIES", ceiling);
            }
        });
    }

    fn git_available() -> bool {
        std::process::Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn setup() -> bool {
        isolate_git_env();
        if git_available() {
            true
        } else {
            kranz_engine::test_capability::skip(
                kranz_engine::test_capability::capability::GIT,
                "git is not on PATH",
            );
            false
        }
    }

    fn raw_git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("spawn git");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Fresh repo on branch `main` with one seed commit; returns
    /// (tempdir, canonicalized root, seed commit sha).
    fn init_repo() -> (TempDir, PathBuf, String) {
        let dir = TempDir::new().unwrap();
        let init = std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(dir.path())
            .output()
            .expect("spawn git init");
        if !init.status.success() {
            raw_git(dir.path(), &["init"]);
            raw_git(dir.path(), &["symbolic-ref", "HEAD", "refs/heads/main"]);
        }
        raw_git(dir.path(), &["config", "user.name", "test"]);
        raw_git(dir.path(), &["config", "user.email", "test@example.com"]);
        std::fs::write(dir.path().join("README.md"), "seed\n").unwrap();
        raw_git(dir.path(), &["add", "-A"]);
        raw_git(dir.path(), &["commit", "-m", "seed"]);
        let root = std::fs::canonicalize(dir.path()).expect("canonicalize repo root");
        let sha = {
            let out = std::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&root)
                .output()
                .expect("rev-parse HEAD");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        (dir, root, sha)
    }

    fn write_events(repo_root: &Path, mission_id: &str, kinds: Vec<EventKind>) {
        let dir = repo_root.join(".kranz").join("missions").join(mission_id);
        std::fs::create_dir_all(&dir).unwrap();
        let mut lines = String::new();
        for (i, kind) in kinds.into_iter().enumerate() {
            let event = Event {
                seq: (i + 1) as u64,
                ts: chrono::Utc::now(),
                mission_id: mission_id.to_string(),
                kind,
            };
            lines.push_str(&serde_json::to_string(&event).unwrap());
            lines.push('\n');
        }
        std::fs::write(dir.join("events.jsonl"), lines).unwrap();
    }

    fn created(mission_branch: &str) -> EventKind {
        EventKind::MissionCreated {
            goal: "fixture mission".to_string(),
            base_branch: "main".to_string(),
            mission_branch: mission_branch.to_string(),
            config: kranz_engine::types::MissionConfig::default(),
        }
    }

    /// Create a Done ticket linked to `mission_id`, with the mission's branch
    /// branched off `base_sha` and (optionally) merged back into `main`
    /// before the mission's events are written as Complete.
    fn scaffold_done_ticket_with_mission(
        repo_root: &Path,
        slug: &str,
        mission_id: &str,
        base_sha: &str,
        merge_into_base: bool,
    ) {
        Ticket::scaffold(repo_root, slug, "fixture ticket", None, None).unwrap();
        Ticket::write_state(repo_root, slug, TicketState::Done, None).unwrap();
        Ticket::record_mission(repo_root, slug, mission_id).unwrap();

        let branch = format!("kranz/mission-{mission_id}");
        raw_git(repo_root, &["checkout", "-b", &branch, base_sha]);
        std::fs::write(repo_root.join("feature.txt"), "new feature\n").unwrap();
        raw_git(repo_root, &["add", "--", "feature.txt"]);
        raw_git(repo_root, &["commit", "-m", "add feature"]);
        raw_git(repo_root, &["checkout", "main"]);
        if merge_into_base {
            raw_git(repo_root, &["merge", "--no-ff", "--no-edit", &branch]);
        }

        write_events(
            repo_root,
            mission_id,
            vec![created(&branch), EventKind::MissionCompleted {}],
        );
    }

    #[test]
    fn cli_ticket_delivered_landed_when_done_and_unmerged() {
        if !setup() {
            return;
        }
        let (_dir, repo_root, base_sha) = init_repo();
        scaffold_done_ticket_with_mission(&repo_root, "unmerged", "m-unmerged", &base_sha, false);

        let label = ticket_terminal_label(&repo_root, "unmerged", TicketState::Done);
        assert_eq!(label, "DELIVERED");

        let ticket = load_ticket(&repo_root, "unmerged").unwrap();
        assert!(render_ticket_show(&ticket, label).contains("[DELIVERED]"));
    }

    #[test]
    fn cli_ticket_delivered_landed_when_done_and_merged() {
        if !setup() {
            return;
        }
        let (_dir, repo_root, base_sha) = init_repo();
        scaffold_done_ticket_with_mission(&repo_root, "merged", "m-merged", &base_sha, true);

        let label = ticket_terminal_label(&repo_root, "merged", TicketState::Done);
        assert_eq!(label, "LANDED");

        let ticket = load_ticket(&repo_root, "merged").unwrap();
        assert!(render_ticket_show(&ticket, label).contains("[LANDED]"));
    }

    #[test]
    fn cli_ticket_delivered_landed_when_done_and_no_mission() {
        if !setup() {
            return;
        }
        let (_dir, repo_root, _base_sha) = init_repo();
        Ticket::scaffold(&repo_root, "no-mission", "fixture ticket", None, None).unwrap();
        Ticket::write_state(&repo_root, "no-mission", TicketState::Done, None).unwrap();

        let label = ticket_terminal_label(&repo_root, "no-mission", TicketState::Done);
        assert_eq!(label, "LANDED", "Done with no linked mission => Landed");
    }

    #[test]
    fn cli_ticket_delivered_landed_leaves_non_terminal_states_unchanged() {
        if !setup() {
            return;
        }
        let (_dir, repo_root, _base_sha) = init_repo();
        Ticket::scaffold(&repo_root, "queued", "fixture ticket", None, None).unwrap();
        Ticket::write_state(&repo_root, "queued", TicketState::Queued, None).unwrap();

        let label = ticket_terminal_label(&repo_root, "queued", TicketState::Queued);
        assert_eq!(label, "QUEUED");
        assert_eq!(label, ticket_state_label(TicketState::Queued));

        for state in [
            TicketState::New,
            TicketState::Drafting,
            TicketState::NeedsContext,
            TicketState::WrongPlan,
            TicketState::Review,
            TicketState::Running,
            TicketState::Failed,
            TicketState::Parked,
        ] {
            assert_eq!(
                ticket_terminal_label(&repo_root, "queued", state),
                ticket_state_label(state),
                "non-Done state {state:?} must render exactly as ticket_state_label"
            );
        }
    }
}

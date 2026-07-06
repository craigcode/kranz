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
        TicketState::Review => "REVIEW",
        TicketState::Queued => "QUEUED",
        TicketState::Running => "RUNNING",
        TicketState::Done => "DONE",
        TicketState::Failed => "FAILED",
    }
}

/// One row of the `kranz ticket list` table: the ticket plus its read state.
pub struct TicketRow<'a> {
    pub ticket: &'a Ticket,
    pub state: TicketState,
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
    let state_w = rows
        .iter()
        .map(|r| ticket_state_label(r.state).len())
        .max()
        .unwrap_or(5)
        .max(5);
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
            ticket_state_label(row.state),
            output::one_line(&row.ticket.title, 60),
        ));
    }
    out
}

/// Render `kranz ticket show <slug>`: the parsed ticket, its state, and any
/// "needs context" block appended to the ticket body.
pub fn render_ticket_show(ticket: &Ticket, state: TicketState) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "ticket {} [{}]\n",
        ticket.slug,
        ticket_state_label(state)
    ));
    out.push_str(&format!("  title:    {}\n", ticket.title));
    out.push_str(&format!("  priority: {}\n", ticket.priority));
    out.push_str(&format!("  schedule: {:?}\n", ticket.schedule));
    if let Some(budget) = ticket.max_budget_usd {
        out.push_str(&format!("  budget:   ${budget:.2}\n"));
    }
    if !ticket.repo_refs.is_empty() {
        out.push_str(&format!("  refs:     {}\n", ticket.repo_refs.join(", ")));
    }

    if !ticket.goal.trim().is_empty() {
        out.push_str("\n## Goal\n");
        out.push_str(ticket.goal.trim());
        out.push('\n');
    }
    if !ticket.context.trim().is_empty() {
        out.push_str("\n## Context\n");
        out.push_str(ticket.context.trim());
        out.push('\n');
    }
    if !ticket.scoping_answers.is_empty() {
        out.push_str("\n## Scoping answers\n");
        for item in &ticket.scoping_answers {
            out.push_str(&format!("- {item}\n"));
        }
    }
    if !ticket.acceptance_hints.is_empty() {
        out.push_str("\n## Acceptance hints\n");
        for item in &ticket.acceptance_hints {
            out.push_str(&format!("- {item}\n"));
        }
    }

    // The "needs context" questions are appended to the raw body by the engine;
    // surface them verbatim so `show` is enough to answer the ticket.
    if let Some(block) = needs_context_block(&ticket.raw_body) {
        out.push('\n');
        out.push_str(&block);
        if !block.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

/// Extract the `## Needs context (from orchestrator)` section (heading +
/// following lines) from a ticket body, if present. Returns everything from
/// that heading to the next `##` heading (or end of body).
fn needs_context_block(body: &str) -> Option<String> {
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
                .starts_with("needs context")
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
        .map(|t| TicketRow {
            ticket: t,
            state: Ticket::read_state(repo, &t.slug),
        })
        .collect();
    render_ticket_list(&rows)
}

/// `kranz ticket show <slug>`.
pub fn cmd_ticket_show(repo: &Path, slug: &str) -> Result<String> {
    let path = Ticket::tickets_dir(repo).join(format!("{slug}.md"));
    if !path.is_file() {
        bail!("ticket '{slug}' not found at {}", path.display());
    }
    let ticket = Ticket::load(&path)?;
    let state = Ticket::read_state(repo, slug);
    Ok(render_ticket_show(&ticket, state))
}

/// `kranz queue`.
pub fn cmd_queue(repo: &Path) -> String {
    let entries = queue::list(repo);
    let busy = queue::is_repo_busy(repo);
    render_queue(&entries, busy.as_deref())
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

/// `kranz draft <slug> [--yes]`: non-interactive plan drafting.
///
/// Sets the ticket Drafting, creates a mission seeded with the whole ticket,
/// requests the plan, and resolves via [`draft_decision`]:
/// - Ready → `approve_plan` (commits plan.md on the mission branch), then set
///   Review (parked) or, with `--yes`, enqueue + set Queued.
/// - NotReady → append the orchestrator's questions to the ticket and set
///   NeedsContext.
///
/// Only the orchestrator runs (no workers); spend is bounded by the
/// orchestrator budget cap (per-ticket override applied).
pub async fn cmd_draft(
    repo: PathBuf,
    slug: &str,
    yes: bool,
    dangerously_allow_all: bool,
) -> Result<i32> {
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
    // out the mission branch to commit plan.md); NeedsContext never touches
    // it, so — matching pre-hoist `cmd_draft` — only restore when a plan was
    // produced. Drop the engine (flush + release the mission lock) first.
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
                println!("  - {q}");
            }
            println!(
                "answer them in {} then run `kranz draft {slug}` again.",
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
pub async fn cmd_work(repo: PathBuf, once: bool) -> Result<i32> {
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

    let report = kranz_engine::work::drain_queue(&repo, once, |mission_id| {
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
    if report.ran.is_empty() && report.skipped.is_empty() {
        println!("queue empty — nothing to do.");
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

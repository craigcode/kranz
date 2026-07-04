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
use anyhow::{anyhow, bail, Context, Result};
use kranz_engine::orchestrator::{MissionEngine, PlanRequest};
use kranz_engine::queue::{self, QueueEntry};
use kranz_engine::ticket::{Ticket, TicketState};
use kranz_engine::types::MissionStatus;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// ticket new — scaffold
// ---------------------------------------------------------------------------

/// The scaffolded body of a new ticket. Frontmatter carries the title,
/// priority and schedule; the body is the four sections the orchestrator
/// expects (`## Goal`, `## Context`, `## Scoping answers`, `## Acceptance
/// hints`), pre-seeded with the goal when one is supplied. The result parses
/// back cleanly through [`Ticket::parse`].
pub fn ticket_template(title: &str, goal: Option<&str>) -> String {
    let goal_body = goal.map(str::trim).filter(|g| !g.is_empty()).unwrap_or("");
    format!(
        "---\n\
         title: {title}\n\
         priority: 2\n\
         schedule: once\n\
         ---\n\
         \n\
         ## Goal\n\
         {goal_body}\n\
         \n\
         ## Context\n\
         \n\
         ## Scoping answers\n\
         \n\
         ## Acceptance hints\n"
    )
}

/// Scaffold `.kranz/tickets/<slug>.md` from the template. Refuses (error) if a
/// ticket with that slug already exists. Returns the written path.
pub fn cmd_ticket_new(
    repo: &Path,
    slug: &str,
    title: &str,
    goal: Option<&str>,
) -> Result<PathBuf> {
    let dir = Ticket::tickets_dir(repo);
    let path = dir.join(format!("{slug}.md"));
    if path.exists() {
        bail!("ticket '{slug}' already exists at {}", path.display());
    }
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    let body = ticket_template(title, goal);
    // Fail loudly if the template ever stops parsing (guards future edits).
    Ticket::parse(slug, &body)
        .map_err(|e| anyhow!("internal error: scaffolded ticket does not parse: {e}"))?;
    std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
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
    out.push_str(&format!("ticket {} [{}]\n", ticket.slug, ticket_state_label(state)));
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
    out.push_str(&format!("{:<3}  {:<3}  {:<14}  {}\n", "#", "PRI", "MISSION", "TICKET"));
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
// draft — pure decision helper
// ---------------------------------------------------------------------------

/// What a `draft` turn resolved to, given the [`PlanRequest`] and whether
/// `--yes` (auto-approve+enqueue) was passed. Separating the decision from the
/// I/O keeps the state-machine unit-testable without a backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DraftDecision {
    /// Plan ready: `approve_plan` (commits plan.md) then set this state.
    /// `Queued` when `--yes` also enqueues; otherwise `Review` (parked).
    Approve { then_enqueue: bool, next_state: TicketState },
    /// Orchestrator wants answers first: append its questions to the ticket
    /// and set `NeedsContext`. Short-circuits before any approval.
    NeedsContext { questions: Vec<String> },
}

/// Map a completed plan request + the `--yes` flag to the next action. Pure:
/// the caller performs the git/state side effects the decision names.
pub fn draft_decision(request: &PlanRequest, yes: bool) -> DraftDecision {
    match request {
        PlanRequest::Ready(_) => DraftDecision::Approve {
            then_enqueue: yes,
            next_state: if yes { TicketState::Queued } else { TicketState::Review },
        },
        PlanRequest::NotReady(text) => DraftDecision::NeedsContext {
            questions: split_questions(text),
        },
    }
}

/// Split the orchestrator's "not ready" prose into individual questions: each
/// non-empty line, with any leading bullet/number marker stripped. A reply
/// with no line breaks becomes a single one-item list.
pub fn split_questions(text: &str) -> Vec<String> {
    let items: Vec<String> = text
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(|l| strip_bullet(l).to_string())
        .filter(|l| !l.is_empty())
        .collect();
    if items.is_empty() {
        // Preserve *something* so the ticket records the orchestrator spoke.
        vec![text.trim().to_string()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect()
    } else {
        items
    }
}

/// Strip a single leading `-`/`*`/`+` bullet or `N.`/`N)` number marker.
fn strip_bullet(line: &str) -> &str {
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(marker) {
            return rest.trim_start();
        }
    }
    // Numbered: leading digits then `.`/`)` then a space.
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i > 0 && i < bytes.len() && (bytes[i] == b'.' || bytes[i] == b')') {
        return line[i + 1..].trim_start();
    }
    line
}

// ---------------------------------------------------------------------------
// work dispatcher — pure decision helper
// ---------------------------------------------------------------------------

/// The dispatcher's next action given the queue front and repo-busy state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkAction {
    /// Nothing queued: the dispatcher exits.
    Empty,
    /// The repo is busy running `mission_id`: wait (default) or exit (`--once`).
    Busy { mission_id: String },
    /// Free to run the front mission.
    Run { mission_id: String, ticket_slug: Option<String> },
}

/// Decide the dispatcher's next step from the queue front + busy state.
/// Pure: `front` is `queue::peek`, `busy_with` is `queue::is_repo_busy`.
pub fn next_work_action(front: Option<&QueueEntry>, busy_with: Option<&str>) -> WorkAction {
    match front {
        None => WorkAction::Empty,
        Some(_) if busy_with.is_some() => WorkAction::Busy {
            mission_id: busy_with.expect("checked is_some").to_string(),
        },
        Some(entry) => WorkAction::Run {
            mission_id: entry.mission_id.clone(),
            ticket_slug: entry.ticket_slug.clone(),
        },
    }
}

/// Map a terminal mission status to the ticket state recorded after a run.
pub fn ticket_state_for_mission(status: MissionStatus) -> TicketState {
    match status {
        MissionStatus::Complete => TicketState::Done,
        // Blocked/Failed/anything-non-complete leaves the ticket Failed so it
        // resurfaces in `ticket list` for a human to pick back up.
        _ => TicketState::Failed,
    }
}

// ---------------------------------------------------------------------------
// Handlers (list/show/new/approve/queue are backend-free; draft/work spawn)
// ---------------------------------------------------------------------------

/// `kranz ticket list`.
pub fn cmd_ticket_list(repo: &Path) -> String {
    let tickets = Ticket::list(repo);
    let rows: Vec<TicketRow<'_>> = tickets
        .iter()
        .map(|t| TicketRow { ticket: t, state: Ticket::read_state(repo, &t.slug) })
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
fn config_for_ticket(mut cfg: kranz_engine::types::MissionConfig, ticket: &Ticket) -> kranz_engine::types::MissionConfig {
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
pub async fn cmd_draft(repo: PathBuf, slug: &str, yes: bool, dangerously_allow_all: bool) -> Result<i32> {
    let ticket = load_ticket(&repo, slug)?;
    let cfg = config_for_ticket(load_config(&repo, dangerously_allow_all)?, &ticket);
    let backend = build_backend(&cfg)?;

    Ticket::write_state(&repo, slug, TicketState::Drafting, None)?;

    let goal = ticket.mission_goal();
    let mut engine = MissionEngine::create(backend, repo.clone(), &goal, cfg)?;
    let mission_id = engine.mission_id().to_string();
    println!("drafting ticket '{slug}' as mission {mission_id}");

    // Seed the orchestrator with the whole ticket, then demand the plan. One
    // planning turn gives it the full context before request_plan.
    if let Err(e) = engine.planning_turn(&goal).await {
        Ticket::write_state(&repo, slug, TicketState::New, None)?;
        return Err(crate::commands::augment_limit_hint(e.into()))
            .with_context(|| format!("seeding the orchestrator for ticket '{slug}'"));
    }
    // Surface any captured seed reply (session start) for visibility.
    if let Some(seed) = engine.take_seed_reply() {
        println!("orchestrator: {}", output::one_line(&seed, 200));
    }

    let request = match engine.request_plan().await {
        Ok(r) => r,
        Err(e) => {
            Ticket::write_state(&repo, slug, TicketState::New, None)?;
            return Err(crate::commands::augment_limit_hint(e.into()))
                .with_context(|| format!("requesting the plan for ticket '{slug}'"));
        }
    };

    match draft_decision(&request, yes) {
        DraftDecision::NeedsContext { questions } => {
            Ticket::append_needs_context(&repo, slug, &questions)?;
            println!("ticket '{slug}' needs context — the orchestrator asked:");
            for q in &questions {
                println!("  - {q}");
            }
            println!(
                "answer them in {} then run `kranz draft {slug}` again.",
                Ticket::tickets_dir(&repo).join(format!("{slug}.md")).display()
            );
            Ok(0)
        }
        DraftDecision::Approve { then_enqueue, next_state } => {
            let PlanRequest::Ready(plan) = request else {
                unreachable!("Approve decision implies a Ready plan");
            };
            println!("{}", output::render_plan(&plan));
            engine.approve_plan(plan)?;
            let branch = engine.state().mission.mission_branch.clone();
            let priority = ticket.priority;
            // Drop the engine (flush + release the mission lock) before touching
            // the queue / running anything.
            drop(engine);

            if then_enqueue {
                queue::enqueue(
                    &repo,
                    QueueEntry {
                        mission_id: mission_id.clone(),
                        ticket_slug: Some(slug.to_string()),
                        priority,
                        seq: 0, // assigned by enqueue
                    },
                )?;
                Ticket::write_state(&repo, slug, next_state, None)?;
                println!(
                    "plan committed on {branch}; mission {mission_id} approved and QUEUED. \
                     Run it with `kranz work`."
                );
            } else {
                Ticket::write_state(&repo, slug, next_state, None)?;
                println!(
                    "plan committed on {branch} for review; mission {mission_id} parked. \
                     Review it, then run `kranz ticket approve {slug}` to queue it \
                     (or `kranz plan --mission {mission_id}` to reshape)."
                );
            }
            Ok(0)
        }
    }
}

/// `kranz ticket approve <slug> [--mission <id>]`: enqueue the parked (Review)
/// mission for the ticket and set the ticket Queued.
///
/// `draft` (no `--yes`) leaves the ticket in Review with a committed plan.md on
/// a mission branch but nothing in the queue. Approving picks that mission:
/// the explicit `--mission` if given, else the newest mission on the repo
/// whose recorded goal equals the ticket's folded [`Ticket::mission_goal`]
/// (that is exactly what `draft` seeded it with).
pub fn cmd_ticket_approve(repo: &Path, slug: &str, explicit_mission: Option<&str>) -> Result<i32> {
    let ticket = load_ticket(repo, slug)?;
    let state = Ticket::read_state(repo, slug);
    if state != TicketState::Review {
        bail!(
            "ticket '{slug}' is {} — only a REVIEW ticket (drafted, plan committed) \
             can be approved; run `kranz draft {slug}` first",
            ticket_state_label(state)
        );
    }
    let mission_id = match explicit_mission {
        Some(id) => id.to_string(),
        None => find_mission_for_ticket(repo, &ticket).ok_or_else(|| {
            anyhow!(
                "could not find the drafted mission for ticket '{slug}' automatically — \
                 pass it with `kranz ticket approve {slug} --mission <id>` (see `kranz missions`)"
            )
        })?,
    };
    let entry = queue::enqueue(
        repo,
        QueueEntry {
            mission_id: mission_id.clone(),
            ticket_slug: Some(slug.to_string()),
            priority: ticket.priority,
            seq: 0,
        },
    )?;
    Ticket::write_state(repo, slug, TicketState::Queued, None)?;
    println!(
        "ticket '{slug}' QUEUED (mission {}, priority {}). Run it with `kranz work`.",
        entry.mission_id, entry.priority
    );
    Ok(0)
}

/// Find the mission `draft` created for a ticket: the newest-by-event-log
/// mission whose recorded goal equals the ticket's folded mission goal.
fn find_mission_for_ticket(repo: &Path, ticket: &Ticket) -> Option<String> {
    use kranz_engine::paths::MissionPaths;
    let goal = ticket.mission_goal();
    let mut best: Option<(std::time::SystemTime, String)> = None;
    for id in MissionPaths::list_missions(repo) {
        let Ok(state) = crate::commands::load_state(repo, &id) else {
            continue;
        };
        if state.mission.goal != goal {
            continue;
        }
        let events = MissionPaths::new(repo, &id).events_file();
        let mtime = std::fs::metadata(&events)
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        if best.as_ref().is_none_or(|(t, _)| mtime >= *t) {
            best = Some((mtime, id));
        }
    }
    best.map(|(_, id)| id)
}

/// `kranz work [--once]`: the dispatcher. Drains the per-repo queue one
/// mission at a time; `--once` processes exactly one front entry (or exits if
/// the repo is busy). Per-repo serialization is enforced by `is_repo_busy`.
pub async fn cmd_work(repo: PathBuf, once: bool) -> Result<i32> {
    loop {
        let front = queue::peek(&repo);
        let busy = queue::is_repo_busy(&repo);
        match next_work_action(front.as_ref(), busy.as_deref()) {
            WorkAction::Empty => {
                println!("queue empty — nothing to do.");
                return Ok(0);
            }
            WorkAction::Busy { mission_id } => {
                println!("repo busy with {mission_id}, waiting");
                if once {
                    // --once must not block: report and exit cleanly.
                    return Ok(0);
                }
                // Poll until the running mission releases the repo. Async
                // sleep so this never blocks a runtime worker thread.
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            }
            WorkAction::Run { mission_id, ticket_slug } => {
                // Claim the entry: remove it so a peer dispatcher won't re-run
                // it, then mark its ticket Running.
                queue::remove(&repo, &mission_id);
                if let Some(slug) = &ticket_slug {
                    Ticket::write_state(&repo, slug, TicketState::Running, None)?;
                }
                println!("running mission {mission_id} from the queue");

                let status = drive_mission(repo.clone(), &mission_id).await;

                if let Some(slug) = &ticket_slug {
                    let (next, ok) = match &status {
                        Ok(code) => (mission_state_from_code(*code), true),
                        Err(_) => (TicketState::Failed, false),
                    };
                    Ticket::write_state(&repo, slug, next, None)?;
                    if !ok {
                        // Surface the error but keep draining the rest.
                        if let Err(e) = status {
                            eprintln!("kranz: mission {mission_id} errored: {e:#}");
                        }
                    }
                } else {
                    status?;
                }

                if once {
                    return Ok(0);
                }
                // Loop: re-check the queue for the next mission.
            }
        }
    }
}

/// Run one queued mission to a terminal state, returning the `run_mission_loop`
/// exit code (0 complete / 2 blocked / 1 failed). Extracted so `cmd_work` maps
/// it to a ticket state.
async fn drive_mission(repo: PathBuf, mission_id: &str) -> Result<i32> {
    run_mission_loop(repo, mission_id.to_string(), false, false).await
}

/// Map a `run_mission_loop` exit code to the ticket's terminal state (0 → Done,
/// anything else → Failed, matching [`ticket_state_for_mission`]).
fn mission_state_from_code(code: i32) -> TicketState {
    if code == 0 {
        TicketState::Done
    } else {
        TicketState::Failed
    }
}

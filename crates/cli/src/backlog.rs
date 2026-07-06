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
use kranz_engine::deps;
use kranz_engine::draft::{drive_draft, DraftOutcome};
use kranz_engine::git_ops::GitRepo;
use kranz_engine::orchestrator::MissionEngine;
use kranz_engine::queue::{self, QueueEntry};
use kranz_engine::ticket::{Ticket, TicketState};
use kranz_engine::types::MissionStatus;
use std::path::{Path, PathBuf};

// Relocated into kranz-engine (roadmap f-1-1): the pure draft decision helpers
// live in `kranz_engine::draft` now so any surface can reuse the sequencing
// core. Re-exported here so existing CLI callers/tests keep working.
pub use kranz_engine::draft::{draft_decision, split_questions, DraftDecision};

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
pub fn cmd_ticket_new(repo: &Path, slug: &str, title: &str, goal: Option<&str>) -> Result<PathBuf> {
    Ticket::ensure_valid_slug(slug)?;
    let dir = Ticket::tickets_dir(repo);
    let path = dir.join(format!("{slug}.md"));
    if path.exists() {
        bail!("ticket '{slug}' already exists at {}", path.display());
    }
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
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
    Run {
        mission_id: String,
        ticket_slug: Option<String>,
    },
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

/// Work-time re-check for a claimed queue entry with a ticket: `Some(blocker)`
/// when one of the ticket's unsatisfied `blocked-by` entries is unsatisfied
/// because that blocker's own ticket ended up Failed (its mission reached a
/// terminal non-Complete state — Failed/Abandoned/Blocked — after
/// batch-approval queued this entry alongside it). The dispatcher must skip
/// such an entry rather than run it: re-driving a mission whose dependency
/// failed can never succeed, and retrying forever would hot-loop.
pub fn work_skip_for_failed_blocker(repo_root: &Path, slug: &str) -> Result<Option<String>> {
    let unsatisfied = deps::unsatisfied_blockers(repo_root, slug)?;
    for blocker in unsatisfied {
        if Ticket::read_state(repo_root, &blocker) == TicketState::Failed {
            return Ok(Some(blocker));
        }
    }
    Ok(None)
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

/// `kranz ticket approve <slug> [--mission <id>]`: enqueue the parked (Review)
/// mission for the ticket and set the ticket Queued.
///
/// `draft` (no `--yes`) leaves the ticket in Review with a committed plan.md on
/// a mission branch but nothing in the queue. Approving picks that mission:
/// the explicit `--mission` if given, else the newest mission on the repo
/// whose recorded goal equals the ticket's folded [`Ticket::mission_goal`]
/// (that is exactly what `draft` seeded it with).
///
/// Before enqueuing, the shared `blocked-by` checks gate approval: a cycle
/// reachable from `slug` is always refused (a structural data error, never
/// overridable by `--force`); an unsatisfied blocker (its mission has not
/// reached [`MissionStatus::Complete`]) is refused unless `force` is set.
pub fn cmd_ticket_approve(
    repo: &Path,
    slug: &str,
    explicit_mission: Option<&str>,
    force: bool,
) -> Result<i32> {
    let ticket = load_ticket(repo, slug)?;
    let state = Ticket::read_state(repo, slug);
    if state != TicketState::Review {
        bail!(
            "ticket '{slug}' is {} — only a REVIEW ticket (drafted, plan committed) \
             can be approved; run `kranz draft {slug}` first",
            ticket_state_label(state)
        );
    }

    if let Some(cycle) = deps::detect_cycle(repo, slug)? {
        bail!("blocked-by cycle: {}", cycle.join(" -> "));
    }
    let unsatisfied = deps::unsatisfied_blockers(repo, slug)?;
    if !unsatisfied.is_empty() && !force {
        bail!(
            "cannot approve {slug}: blocked by {} (its mission is not Complete)",
            unsatisfied.join(", ")
        );
    }

    let mission_id = match explicit_mission {
        Some(id) => id.to_string(),
        None => Ticket::mission_for(repo, slug)
            .or_else(|| find_mission_for_ticket(repo, &ticket))
            .ok_or_else(|| {
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

/// Legacy fallback when no recorded link exists (missions drafted before the
/// sidecar carried `missionId`): newest mission whose goal matches.
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
    // Remember the operator's checkout: each mission's run() asserts its own
    // branch, so when the dispatcher exits it puts the checkout back where
    // the operator started (tracked-dirty trees abort the restore).
    let dispatch_branch = GitRepo::open(&repo)
        .ok()
        .and_then(|g| g.current_branch().ok());
    // Claims abandoned by a crashed dispatcher come back first (review P1).
    let recovered = queue::recover_dead_claims(&repo);
    if recovered > 0 {
        println!(
            "recovered {recovered} claimed queue entr{} from dead dispatchers",
            if recovered == 1 { "y" } else { "ies" }
        );
    }
    loop {
        let front = queue::peek(&repo);
        let busy = queue::is_repo_busy(&repo);
        match next_work_action(front.as_ref(), busy.as_deref()) {
            WorkAction::Empty => {
                println!("queue empty — nothing to do.");
                restore_work_checkout(&repo, dispatch_branch.as_deref());
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
            WorkAction::Run {
                mission_id: _,
                ticket_slug,
            } => {
                // Claim the entry ATOMICALLY (rename, not remove): a peer
                // dispatcher can't double-run it, and a crash here leaves a
                // recoverable claim file instead of dropped work (review P1).
                let Some(claim) = queue::claim_front(&repo) else {
                    // Raced with a sibling (or the queue is transiently
                    // unclaimable): brief pause so this can never hot-spin.
                    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                    continue;
                };
                // The front may have moved between peek and claim — the
                // CLAIMED entry is authoritative, so run that one.
                let mission_id = claim.entry.mission_id.clone();
                if let Some(slug) = &ticket_slug {
                    // Re-check blockers at work time: batch-approval can queue
                    // a dependent alongside a blocker that fails before its
                    // turn comes up. Retire the claim (never release it — that
                    // would re-claim this same doomed entry forever) and mark
                    // the ticket Failed with a note naming the blocker.
                    if let Some(blocker) = work_skip_for_failed_blocker(&repo, slug)? {
                        queue::finish_claim(claim);
                        Ticket::write_state(
                            &repo,
                            slug,
                            TicketState::Failed,
                            Some(format!("skipped: blocked-by {blocker} failed")),
                        )?;
                        eprintln!(
                            "warning: skipping ticket '{slug}' — blocked-by '{blocker}' failed"
                        );
                        continue;
                    }
                    Ticket::write_state(&repo, slug, TicketState::Running, None)?;
                }
                println!("running mission {mission_id} from the queue");

                let status = drive_mission(repo.clone(), &mission_id).await;

                // Terminal outcome (any) retires the claim. A mission we
                // could not RUN (lock held, config, spawn failure): for a
                // ticket-born entry the ticket is marked Failed below and the
                // claim retires with it (matching pre-claim semantics, no
                // re-run loop); a bare entry is RELEASED so the work isn't
                // lost, and the `status?` below stops this dispatcher rather
                // than hot-looping on the same failing entry.
                match &status {
                    Ok(_) => queue::finish_claim(claim),
                    Err(_) if ticket_slug.is_some() => queue::finish_claim(claim),
                    Err(_) => queue::release_claim(claim),
                }

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
                    restore_work_checkout(&repo, dispatch_branch.as_deref());
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
    run_mission_loop(
        repo,
        mission_id.to_string(),
        kranz_engine::event_log::LockForce::No,
        false,
    )
    .await
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

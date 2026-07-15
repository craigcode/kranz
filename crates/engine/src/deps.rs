//! The `blocked-by` dependency primitive: shared checks the CLI and REST
//! approve paths both call so a ticket can't be approved (or drafted into a
//! cycle) while a dependency is outstanding.

use crate::error::{EngineError, Result};
use crate::event_log::EventLog;
use crate::paths::MissionPaths;
use crate::queue::{self, QueueEntry};
use crate::reducer;
use crate::ticket::{Ticket, TicketState};
use crate::types::MissionStatus;
use std::collections::HashSet;
use std::path::Path;

/// The current status of a mission by id, read by folding its event log.
/// `None` when the mission has no event log at all (never drafted, or the
/// slug's recorded `missionId` is stale).
fn mission_status(repo_root: &Path, mission_id: &str) -> Option<MissionStatus> {
    let paths = MissionPaths::new(repo_root, mission_id);
    let events = EventLog::read_events(&paths.events_file()).ok()?;
    let state = reducer::fold(&events).ok()?;
    Some(state.mission.status)
}

/// Blocker slugs for `slug` whose mission has not reached
/// [`MissionStatus::Complete`]. A blocker with no ticket file, no recorded
/// mission, or any non-Complete status counts as unsatisfied — satisfaction
/// is authoritative on mission status, never on ticket/queue state.
pub fn unsatisfied_blockers(repo_root: &Path, slug: &str) -> Result<Vec<String>> {
    Ticket::ensure_valid_slug(slug)?;
    let path = Ticket::tickets_dir(repo_root).join(format!("{slug}.md"));
    let ticket = Ticket::load(&path)?;

    let mut unsatisfied = Vec::new();
    for blocker in &ticket.blocked_by {
        Ticket::ensure_valid_slug(blocker)?;
        let satisfied = Ticket::mission_for(repo_root, blocker)
            .and_then(|mission_id| mission_status(repo_root, &mission_id))
            .is_some_and(|status| status == MissionStatus::Complete);
        if !satisfied {
            unsatisfied.push(blocker.clone());
        }
    }
    Ok(unsatisfied)
}

/// Single source of blocked-ness. Approve gates (dashboard + Slack) and
/// every rendering surface MUST call this, never re-derive.
pub fn is_blocked(repo_root: &Path, slug: &str) -> Result<bool> {
    Ok(!unsatisfied_blockers(repo_root, slug)?.is_empty())
}

/// DFS the `blocked-by` edges across ticket files starting from `slug`. When
/// a cycle is reachable, returns `Some(path)` listing the slugs that form it
/// in order (e.g. `[a, b, a]`); a ticket file missing along the way
/// terminates that branch since it cannot extend a cycle.
pub fn detect_cycle(repo_root: &Path, slug: &str) -> Result<Option<Vec<String>>> {
    Ticket::ensure_valid_slug(slug)?;
    let mut path = vec![slug.to_string()];
    let mut on_path: HashSet<String> = HashSet::new();
    on_path.insert(slug.to_string());
    dfs(repo_root, slug, &mut path, &mut on_path)
}

fn dfs(
    repo_root: &Path,
    current: &str,
    path: &mut Vec<String>,
    on_path: &mut HashSet<String>,
) -> Result<Option<Vec<String>>> {
    let ticket_path = Ticket::tickets_dir(repo_root).join(format!("{current}.md"));
    let Ok(ticket) = Ticket::load(&ticket_path) else {
        // No ticket file here: this branch cannot extend a cycle.
        return Ok(None);
    };

    for blocker in &ticket.blocked_by {
        Ticket::ensure_valid_slug(blocker)?;

        if on_path.contains(blocker) {
            let mut cycle = path.clone();
            cycle.push(blocker.clone());
            let start = cycle
                .iter()
                .position(|s| s == blocker)
                .expect("blocker is in on_path, so it is in path");
            return Ok(Some(cycle[start..].to_vec()));
        }

        path.push(blocker.clone());
        on_path.insert(blocker.clone());
        if let Some(cycle) = dfs(repo_root, blocker, path, on_path)? {
            return Ok(Some(cycle));
        }
        path.pop();
        on_path.remove(blocker);
    }

    Ok(None)
}

// ---------------------------------------------------------------------------
// approve_ticket — the shared gate + side effects (CLI `ticket approve` and
// REST `POST /api/tickets/:slug/approve`)
// ---------------------------------------------------------------------------

/// A ticket approved into the queue: the mission id and priority it was
/// enqueued with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovedTicket {
    pub mission_id: String,
    pub priority: u8,
}

/// The one approve gate + enqueue side effect shared by the CLI (`kranz
/// ticket approve`) and the REST `POST /api/tickets/:slug/approve` handler,
/// so the two surfaces can never drift apart on what "approvable" means.
///
/// Refuses (via [`EngineError::InvalidState`]) when: the ticket is not
/// [`TicketState::Review`] or [`TicketState::Parked`]; a `blocked-by` cycle
/// is reachable from `slug` (never overridable by `force`); or an
/// unsatisfied blocker exists and `force` is false. On success, enqueues the
/// ticket's drafted mission (`explicit_mission`, else the recorded/discovered
/// one) and sets the ticket [`TicketState::Queued`].
///
/// [`TicketState::Parked`] is accepted so a readiness park can be re-queued
/// after the operator fixes auth/binaries — the plan is already committed.
pub fn approve_ticket(
    repo_root: &Path,
    slug: &str,
    explicit_mission: Option<&str>,
    force: bool,
) -> Result<ApprovedTicket> {
    Ticket::ensure_valid_slug(slug)?;
    let ticket_path = Ticket::tickets_dir(repo_root).join(format!("{slug}.md"));
    let ticket = Ticket::load(&ticket_path)?;

    let state = Ticket::read_state(repo_root, slug);
    if !matches!(state, TicketState::Review | TicketState::Parked) {
        return Err(EngineError::InvalidState(format!(
            "ticket '{slug}' is {} — only a REVIEW or PARKED ticket \
             can be queued; run `kranz draft {slug}` first",
            ticket_state_label(state)
        )));
    }

    if let Some(cycle) = detect_cycle(repo_root, slug)? {
        return Err(EngineError::InvalidState(format!(
            "blocked-by cycle: {}",
            cycle.join(" -> ")
        )));
    }
    let unsatisfied = unsatisfied_blockers(repo_root, slug)?;
    if !unsatisfied.is_empty() && !force {
        return Err(EngineError::InvalidState(format!(
            "cannot approve {slug}: blocked by {} (its mission is not Complete)",
            unsatisfied.join(", ")
        )));
    }

    let mission_id = match explicit_mission {
        Some(id) => id.to_string(),
        None => Ticket::mission_for(repo_root, slug)
            .or_else(|| find_mission_for_ticket(repo_root, &ticket))
            .ok_or_else(|| {
                EngineError::InvalidState(format!(
                    "could not find the drafted mission for ticket '{slug}' automatically — \
                     pass one explicitly (see `kranz missions`)"
                ))
            })?,
    };

    let entry = queue::enqueue(
        repo_root,
        QueueEntry {
            mission_id,
            ticket_slug: Some(slug.to_string()),
            priority: ticket.priority,
            seq: 0,
        },
    )?;
    Ticket::write_state(repo_root, slug, TicketState::Queued, None)?;
    Ok(ApprovedTicket {
        mission_id: entry.mission_id,
        priority: entry.priority,
    })
}

/// UPPERCASE label for a ticket pipeline state, matching
/// `kranz_cli::backlog::ticket_state_label` — duplicated here (rather than
/// depended on) since the CLI crate depends on this one, not the reverse.
/// Only feeds internal error messaging (never a terminal/Done label surfaced
/// to an operator); the CLI's Delivered/Landed split for `Done` lives
/// entirely in `kranz_cli::backlog::ticket_terminal_label`.
fn ticket_state_label(state: TicketState) -> &'static str {
    match state {
        TicketState::New => "NEW",
        TicketState::Drafting => "DRAFTING",
        TicketState::NeedsContext => "NEEDS-CONTEXT",
        TicketState::Review => "REVIEW",
        TicketState::Queued => "QUEUED",
        TicketState::Running => "RUNNING",
        TicketState::Done => "DONE",
        TicketState::Failed => "FAILED",
        TicketState::Parked => "PARKED",
    }
}

/// Legacy fallback when no recorded link exists (missions drafted before the
/// sidecar carried `missionId`): newest mission whose goal matches.
fn find_mission_for_ticket(repo_root: &Path, ticket: &Ticket) -> Option<String> {
    let goal = ticket.mission_goal();
    let mut best: Option<(std::time::SystemTime, String)> = None;
    for id in MissionPaths::list_missions(repo_root) {
        let paths = MissionPaths::new(repo_root, &id);
        let Ok(events) = EventLog::read_events(&paths.events_file()) else {
            continue;
        };
        let Ok(state) = reducer::fold(&events) else {
            continue;
        };
        if state.mission.goal != goal {
            continue;
        }
        let mtime = std::fs::metadata(paths.events_file())
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        if best.as_ref().is_none_or(|(t, _)| mtime >= *t) {
            best = Some((mtime, id));
        }
    }
    best.map(|(_, id)| id)
}

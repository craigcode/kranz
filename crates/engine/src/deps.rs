//! The `blocked-by` dependency primitive: shared checks the CLI and REST
//! approve paths both call so a ticket can't be approved (or drafted into a
//! cycle) while a dependency is outstanding.

use crate::error::Result;
use crate::event_log::EventLog;
use crate::paths::MissionPaths;
use crate::reducer;
use crate::ticket::Ticket;
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

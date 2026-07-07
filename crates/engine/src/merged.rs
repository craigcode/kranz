//! Shared merge-ancestry derivation, reused by mission rows (REST
//! `/api/missions`) and the ticket projection's Delivered/Landed split.
//!
//! ONE probe (`git_ops::is_ancestor` via [`merged_bit`]) backs both call
//! sites — no separate git logic per surface.

use crate::event_log::EventLog;
use crate::git_ops::GitRepo;
use crate::paths::MissionPaths;
use crate::reducer;
use crate::ticket::{Ticket, TicketState};
use crate::types::{Mission, MissionStatus};
use std::path::Path;

/// Whether `mission`'s branch tip is an ancestor of the LIVE base branch tip
/// (not the pinned `base_sha` — merged-detection tracks whatever the base
/// branch has absorbed as of now). `None` when there is no mission branch, or
/// when any ref fails to resolve; a per-mission git failure here must not
/// fail the whole list.
pub fn merged_bit(repo: &GitRepo, mission: &Mission) -> Option<bool> {
    if !repo.branch_exists(&mission.mission_branch).ok()? {
        return None;
    }
    let mission_tip = repo.rev_parse(&mission.mission_branch).ok()?;
    let base_tip = repo.rev_parse(&mission.base_branch).ok()?;
    repo.is_ancestor(&mission_tip, &base_tip).ok()
}

/// The Delivered/Landed merge status of a `Done` ticket, or `None` when the
/// split doesn't apply.
///
/// Caller semantics (mirrors `apps/dashboard/src/lib/pipelineStage.ts:87-96`
/// exactly): `Some(false)` => Delivered (mission complete but its branch is
/// not yet merged); `Some(true)` => Landed (mission branch merged into
/// base); `None` => split not applicable — callers treat a Done ticket with
/// `None` as Landed (direct-fixed with no linked mission, or a mission that
/// is dead/unloadable).
pub fn ticket_merged(repo_root: &Path, slug: &str) -> Option<bool> {
    if Ticket::read_state(repo_root, slug) != TicketState::Done {
        return None;
    }
    let mission_id = Ticket::mission_for(repo_root, slug)?;
    let paths = MissionPaths::new(repo_root, &mission_id);
    if !paths.events_file().is_file() {
        return None;
    }
    let events = EventLog::read_events(&paths.events_file()).ok()?;
    let state = reducer::fold(&events).ok()?;
    if state.mission.status != MissionStatus::Complete {
        return None;
    }
    let repo = GitRepo::open(repo_root).ok()?;
    merged_bit(&repo, &state.mission)
}

//! Host-callable queue drain core (roadmap f-1-1): the drain/claim/skip loop
//! that used to live only inside `kranz` CLI's `cmd_work`, hoisted here so any
//! surface (CLI, REST, Slack) can drain a repo's queue.
//!
//! [`drain_queue`] owns the loop — recover dead claims, then peek/claim/run
//! one mission at a time — but does NOT run missions itself: the caller
//! injects a `run_mission` closure, because each host supplies its own runner
//! (the CLI tails events to stderr; a headless caller does not) and only the
//! caller knows how to restore its own git checkout.

use crate::backend_readiness::{self, DrainDecision};
use crate::deps;
use crate::event_log::EventLog;
use crate::paths::MissionPaths;
use crate::queue::{self, QueueEntry};
use crate::reducer;
use crate::ticket::{Ticket, TicketState};
use crate::types::MissionStatus;
use anyhow::Result;
use std::future::Future;
use std::path::Path;

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

/// Map a terminal (or blocked) mission status to the ticket state recorded
/// after a run. Only called by [`reconcile_ticket_for_mission`] for
/// terminal/blocked statuses — live statuses are gated out before this runs.
pub fn ticket_state_for_mission(status: MissionStatus) -> TicketState {
    match status {
        MissionStatus::Complete => TicketState::Done,
        MissionStatus::Failed => TicketState::Failed,
        MissionStatus::Abandoned => TicketState::Failed,
        // Blocked is needs-input, not a failure: the mission is waiting on a
        // human, so the ticket should resurface as NeedsContext, not Failed.
        MissionStatus::Blocked => TicketState::NeedsContext,
        _ => TicketState::Failed,
    }
}

/// The single authoritative reconcile helper: given a mission id, reverse-
/// looks-up its linked ticket and, if the mission's folded status is
/// terminal-or-blocked, writes the mapped [`TicketState`] to the ticket's
/// `.status` sidecar. LIVE statuses (Running/Validating/Paused/Approved/
/// Planning) are a no-op — the ticket is still mid-flight and must not be
/// clobbered. Every path that can drive a mission to a terminal (or blocked)
/// state — `kranz run`, `kranz exec`, REST `/start`, the drain loop — should
/// call this instead of writing the ticket state itself, so the stale-
/// "Failed" heal case and the Blocked-to-NeedsContext mapping live in one
/// place.
///
/// Defensive by design (mirrors [`crate::merged::ticket_merged`]): an
/// unlinked or unloadable mission is `Ok(None)`, never an error — reconcile
/// must never fail the caller's terminal-state transition.
pub fn reconcile_ticket_for_mission(
    repo_root: &Path,
    mission_id: &str,
) -> crate::error::Result<Option<(String, TicketState)>> {
    let Some(slug) = Ticket::slug_for_mission(repo_root, mission_id) else {
        return Ok(None);
    };

    let paths = MissionPaths::new(repo_root, mission_id);
    if !paths.events_file().is_file() {
        return Ok(None);
    }
    let Ok(events) = EventLog::read_events(&paths.events_file()) else {
        return Ok(None);
    };
    let Ok(state) = reducer::fold(&events) else {
        return Ok(None);
    };

    let status = state.mission.status;
    if !matches!(
        status,
        MissionStatus::Complete
            | MissionStatus::Failed
            | MissionStatus::Abandoned
            | MissionStatus::Blocked
    ) {
        return Ok(None);
    }

    let mapped = ticket_state_for_mission(status);
    let current = Ticket::read_state(repo_root, &slug);
    if current == mapped {
        return Ok(None);
    }
    Ticket::write_state(repo_root, &slug, mapped, None)?;
    Ok(Some((slug, mapped)))
}

// ---------------------------------------------------------------------------
// drain_queue — the shared drain/claim/skip loop
// ---------------------------------------------------------------------------

/// Outcome of a [`drain_queue`] call: the mission ids that ran to a terminal
/// state vs. those skipped because a blocker failed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DrainReport {
    pub ran: Vec<String>,
    pub skipped: Vec<String>,
    /// Claimed then parked because backend readiness failed hard (ticket
    /// marked [`TicketState::Parked`] with a readiness note when linked).
    pub parked: Vec<String>,
    /// `once` stopped the drain while the repo was busy with another mission
    /// (nothing here was claimed or run). Callers that restore their own git
    /// checkout on exit must NOT do so when this is set — the repo is still
    /// mid-mission under a sibling dispatcher, and switching branches out
    /// from under it would corrupt that run's working tree.
    pub stopped_busy: bool,
}

/// After this many consecutive rate-limit delays on the same mission id in one
/// drain, park instead of starving the rest of the queue forever.
const RATE_LIMIT_ROTATE_CAP: u32 = 3;

/// Drain the per-repo queue one mission at a time. Recover dead claims once up
/// front; then atomically claim the front entry with the repo-wide busy guard;
/// on `Busy` either return (`once`) or sleep 5s and retry; on a lost claim race,
/// retry after a brief sleep; on `Claimed`, run backend readiness under the
/// claim (park → finish_claim; rate-limit → release + rotate/delay; ok → run),
/// skip a ticket-born entry whose blocker failed, otherwise write ticket
/// Running, invoke the injected `run_mission`, then finish/release the claim,
/// write the terminal ticket state, and honor `once`.
///
/// Readiness is probed **after** claim so a sibling drain cannot race a peek
/// + `queue::remove` into marking a live run's ticket Failed.
///
/// Does NOT do checkout restoration or event printing — those stay with the
/// caller, which is exactly why `run_mission` is injected rather than run
/// inside this core: the CLI keeps its live event tail, while a headless
/// caller (REST, Slack) can drive the same loop with no terminal attached.
pub async fn drain_queue<R, Fut>(
    repo_root: &Path,
    once: bool,
    run_mission: R,
) -> Result<DrainReport>
where
    R: Fn(String) -> Fut,
    Fut: Future<Output = Result<i32>>,
{
    drain_queue_with_probe(
        repo_root,
        once,
        run_mission,
        backend_readiness::probe_mission,
    )
    .await
}

/// [`drain_queue`] with an injectable readiness probe. Engine and host tests
/// use this seam to script proceed / park / rate-limit decisions without
/// shelling out to whichever agent CLIs happen to be installed on the test
/// machine.
pub async fn drain_queue_with_probe<R, Fut, P>(
    repo_root: &Path,
    once: bool,
    run_mission: R,
    probe: P,
) -> Result<DrainReport>
where
    R: Fn(String) -> Fut,
    Fut: Future<Output = Result<i32>>,
    P: Fn(&Path, &str) -> crate::error::Result<backend_readiness::ReadinessReport>,
{
    let mut report = DrainReport::default();
    let mut rate_limit_hits: std::collections::HashMap<String, u32> =
        std::collections::HashMap::new();
    queue::recover_dead_claims(repo_root);
    loop {
        match queue::claim_front_when_repo_free(repo_root)? {
            queue::ClaimFront::Empty => return Ok(report),
            queue::ClaimFront::LostRace => {
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                continue;
            }
            queue::ClaimFront::Busy { .. } => {
                if once {
                    report.stopped_busy = true;
                    return Ok(report);
                }
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            }
            queue::ClaimFront::Claimed(claim) => {
                let mission_id = claim.entry.mission_id.clone();
                let ticket_slug = claim
                    .entry
                    .ticket_slug
                    .clone()
                    .or_else(|| Ticket::slug_for_mission(repo_root, &mission_id));

                // Backend readiness under the claim (no peek/remove race).
                match probe(repo_root, &mission_id) {
                    Ok(readiness) => match readiness.drain_decision() {
                        DrainDecision::Proceed { warnings } => {
                            for w in warnings {
                                tracing::warn!(
                                    mission = %mission_id,
                                    warning = %w,
                                    "backend readiness warning; proceeding"
                                );
                            }
                        }
                        DrainDecision::Park { reason } => {
                            tracing::warn!(
                                mission = %mission_id,
                                reason = %reason,
                                "parking claimed mission: backend not ready"
                            );
                            queue::finish_claim(claim);
                            if let Some(slug) = &ticket_slug {
                                Ticket::write_state(
                                    repo_root,
                                    slug,
                                    TicketState::Parked,
                                    Some(format!("parked (backend not ready): {reason}")),
                                )?;
                            }
                            report.parked.push(mission_id);
                            if once {
                                return Ok(report);
                            }
                            continue;
                        }
                        DrainDecision::RequeueDelay { reason, delay } => {
                            let hits = rate_limit_hits.entry(mission_id.clone()).or_insert(0);
                            *hits += 1;
                            let hits = *hits;
                            tracing::warn!(
                                mission = %mission_id,
                                reason = %reason,
                                delay_secs = delay.as_secs(),
                                hits,
                                "backend rate-limited after claim"
                            );
                            if hits >= RATE_LIMIT_ROTATE_CAP {
                                queue::finish_claim(claim);
                                if let Some(slug) = &ticket_slug {
                                    Ticket::write_state(
                                        repo_root,
                                        slug,
                                        TicketState::Parked,
                                        Some(format!("parked (rate-limited {hits}×): {reason}")),
                                    )?;
                                }
                                report.parked.push(mission_id);
                                if once {
                                    return Ok(report);
                                }
                                continue;
                            }
                            // Release back to the queue, then rotate behind
                            // other same-priority work so one limited head
                            // cannot starve the drain forever.
                            let entry = claim.entry.clone();
                            queue::release_claim(claim);
                            rotate_entry_to_back(repo_root, &entry)?;
                            if once {
                                return Ok(report);
                            }
                            // Production delay is typically 60s; tests clamp so
                            // rate-limit rotate/park coverage stays hermetic.
                            #[cfg(test)]
                            let delay = delay.min(std::time::Duration::from_millis(1));
                            tokio::time::sleep(delay).await;
                            continue;
                        }
                    },
                    Err(e) => {
                        tracing::warn!(
                            mission = %mission_id,
                            error = %e,
                            "backend readiness probe failed; proceeding"
                        );
                    }
                }

                // Disk-footprint preflight (ticket mission-build-footprint):
                // refuse to START a mission whose build ladder won't fit in
                // the free space under the repo, naming the estimate, rather
                // than dying mid-feature with os error 28. Unmeasurable free
                // space degrades to proceed (never a fabricated refusal).
                if let crate::disk_preflight::DiskPreflight::Insufficient {
                    free_bytes,
                    estimate_bytes,
                } = crate::disk_preflight::check(repo_root)
                {
                    let reason = format!(
                        "insufficient disk for the mission build ladder: {} free < {} estimated \
                         (free space or raise the estimate)",
                        crate::disk_preflight::gib(free_bytes),
                        crate::disk_preflight::gib(estimate_bytes)
                    );
                    tracing::warn!(mission = %mission_id, reason = %reason, "parking claimed mission: disk preflight");
                    queue::finish_claim(claim);
                    if let Some(slug) = &ticket_slug {
                        Ticket::write_state(
                            repo_root,
                            slug,
                            TicketState::Parked,
                            Some(format!("parked (disk): {reason}")),
                        )?;
                    }
                    report.parked.push(mission_id);
                    if once {
                        return Ok(report);
                    }
                    continue;
                }

                if let Some(slug) = &ticket_slug {
                    if let Some(blocker) = work_skip_for_failed_blocker(repo_root, slug)? {
                        queue::finish_claim(claim);
                        Ticket::write_state(
                            repo_root,
                            slug,
                            TicketState::Failed,
                            Some(format!("skipped: blocked-by {blocker} failed")),
                        )?;
                        report.skipped.push(mission_id);
                        continue;
                    }
                    Ticket::write_state(repo_root, slug, TicketState::Running, None)?;
                }

                let status = run_mission(mission_id.clone()).await;

                match &status {
                    Ok(_) => queue::finish_claim(claim),
                    Err(_) if ticket_slug.is_some() => queue::finish_claim(claim),
                    Err(_) => queue::release_claim(claim),
                }

                if ticket_slug.is_some() {
                    reconcile_ticket_for_mission(repo_root, &mission_id)?;
                } else {
                    status?;
                }
                report.ran.push(mission_id);

                if once {
                    return Ok(report);
                }
            }
        }
    }
}

/// Drop `entry` from the queue (if still present) and re-enqueue so it gets a
/// fresh seq and sorts behind other same-priority work.
fn rotate_entry_to_back(repo_root: &Path, entry: &QueueEntry) -> Result<()> {
    queue::remove(repo_root, &entry.mission_id);
    let _ = queue::enqueue(
        repo_root,
        QueueEntry {
            mission_id: entry.mission_id.clone(),
            ticket_slug: entry.ticket_slug.clone(),
            priority: entry.priority,
            // seq overwritten by enqueue
            seq: 0,
        },
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{Event, EventKind};
    use crate::types::MissionConfig;
    use chrono::Utc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn next_work_action_empty_when_no_front() {
        assert_eq!(next_work_action(None, None), WorkAction::Empty);
    }

    #[test]
    fn next_work_action_busy_takes_priority_over_front() {
        let entry = QueueEntry {
            mission_id: "m1".to_string(),
            ticket_slug: None,
            priority: 2,
            seq: 0,
        };
        assert_eq!(
            next_work_action(Some(&entry), Some("busy-mission")),
            WorkAction::Busy {
                mission_id: "busy-mission".to_string()
            }
        );
    }

    #[test]
    fn next_work_action_runs_the_front_when_idle() {
        let entry = QueueEntry {
            mission_id: "m1".to_string(),
            ticket_slug: Some("slug-a".to_string()),
            priority: 2,
            seq: 0,
        };
        assert_eq!(
            next_work_action(Some(&entry), None),
            WorkAction::Run {
                mission_id: "m1".to_string(),
                ticket_slug: Some("slug-a".to_string()),
            }
        );
    }

    #[test]
    fn ticket_state_for_mission_maps_terminal_status() {
        assert_eq!(
            ticket_state_for_mission(MissionStatus::Complete),
            TicketState::Done
        );
        assert_eq!(
            ticket_state_for_mission(MissionStatus::Blocked),
            TicketState::NeedsContext
        );
        assert_eq!(
            ticket_state_for_mission(MissionStatus::Failed),
            TicketState::Failed
        );
        assert_eq!(
            ticket_state_for_mission(MissionStatus::Abandoned),
            TicketState::Failed
        );
    }

    fn write_ticket(repo: &Path, slug: &str, body: &str) {
        let dir = Ticket::tickets_dir(repo);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{slug}.md")), body).unwrap();
    }

    /// Write a hand-built events.jsonl for `mission_id` under `repo_root`
    /// (mirrors `merged_test.rs::write_events`).
    fn write_events(repo_root: &Path, mission_id: &str, kinds: Vec<EventKind>) {
        let dir = repo_root.join(".kranz").join("missions").join(mission_id);
        std::fs::create_dir_all(&dir).unwrap();
        let mut lines = String::new();
        for (i, kind) in kinds.into_iter().enumerate() {
            let event = Event {
                seq: (i + 1) as u64,
                ts: Utc::now(),
                mission_id: mission_id.to_string(),
                kind,
            };
            lines.push_str(&serde_json::to_string(&event).unwrap());
            lines.push('\n');
        }
        std::fs::write(dir.join("events.jsonl"), lines).unwrap();
    }

    fn created() -> EventKind {
        EventKind::MissionCreated {
            goal: "fixture mission".to_string(),
            base_branch: "main".to_string(),
            mission_branch: "kranz/mission-fixture".to_string(),
            config: MissionConfig::default(),
        }
    }

    fn scaffold_ticket(repo_root: &Path, slug: &str, mission_id: &str, state: TicketState) {
        Ticket::scaffold(repo_root, slug, "fixture ticket", None, None).unwrap();
        Ticket::record_mission(repo_root, slug, mission_id).unwrap();
        Ticket::write_state(repo_root, slug, state, None).unwrap();
    }

    fn plan_with_one_milestone() -> crate::types::Plan {
        crate::types::Plan {
            goal: "fixture goal".to_string(),
            validation_contract: vec![],
            milestones: vec![crate::types::PlanMilestone {
                title: "milestone one".to_string(),
                features: vec![crate::types::PlanFeature {
                    title: "feature one".to_string(),
                    spec: "spec".to_string(),
                    validation_criteria: vec!["works".to_string()],
                }],
            }],
            considered_alternatives: None,
            command_grants: vec![],
            touch_set: vec![],
        }
    }

    #[test]
    fn reconcile_on_terminal_maps_complete_to_done() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        scaffold_ticket(repo, "my-ticket", "m1", TicketState::Running);
        write_events(repo, "m1", vec![created(), EventKind::MissionCompleted {}]);

        let result = reconcile_ticket_for_mission(repo, "m1").unwrap();
        assert_eq!(result, Some(("my-ticket".to_string(), TicketState::Done)));
        assert_eq!(Ticket::read_state(repo, "my-ticket"), TicketState::Done);
    }

    #[test]
    fn reconcile_heals_failed_to_done() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        scaffold_ticket(repo, "my-ticket", "m1", TicketState::Failed);
        write_events(repo, "m1", vec![created(), EventKind::MissionCompleted {}]);

        let result = reconcile_ticket_for_mission(repo, "m1").unwrap();
        assert_eq!(result, Some(("my-ticket".to_string(), TicketState::Done)));
        assert_eq!(Ticket::read_state(repo, "my-ticket"), TicketState::Done);
    }

    #[test]
    fn blocked_reconciles_to_needs_you() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        scaffold_ticket(repo, "my-ticket", "m1", TicketState::Running);
        write_events(
            repo,
            "m1",
            vec![
                created(),
                EventKind::PlanApproved {
                    plan: plan_with_one_milestone(),
                    base_sha: None,
                },
                EventKind::MilestoneBlocked {
                    milestone_id: "ms-1".to_string(),
                    reason: "needs input".to_string(),
                },
            ],
        );

        let result = reconcile_ticket_for_mission(repo, "m1").unwrap();
        assert_eq!(
            result,
            Some(("my-ticket".to_string(), TicketState::NeedsContext))
        );
        assert_eq!(
            Ticket::read_state(repo, "my-ticket"),
            TicketState::NeedsContext
        );
    }

    #[test]
    fn reconcile_returns_none_when_no_linked_ticket() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        write_events(repo, "m1", vec![created(), EventKind::MissionCompleted {}]);

        assert_eq!(reconcile_ticket_for_mission(repo, "m1").unwrap(), None);
    }

    #[test]
    fn reconcile_returns_none_when_mission_status_is_live() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        scaffold_ticket(repo, "my-ticket", "m1", TicketState::Running);
        write_events(repo, "m1", vec![created()]);

        assert_eq!(reconcile_ticket_for_mission(repo, "m1").unwrap(), None);
        assert_eq!(Ticket::read_state(repo, "my-ticket"), TicketState::Running);
    }

    fn proceed_report(mission_id: &str) -> backend_readiness::ReadinessReport {
        backend_readiness::ReadinessReport {
            mission_id: mission_id.to_string(),
            roles: vec![],
            overall: backend_readiness::ReadinessStatus::Ok,
            warnings: vec![],
        }
    }

    fn always_proceed(
        _repo: &Path,
        id: &str,
    ) -> crate::error::Result<backend_readiness::ReadinessReport> {
        Ok(proceed_report(id))
    }

    fn park_report(mission_id: &str) -> backend_readiness::ReadinessReport {
        backend_readiness::ReadinessReport {
            mission_id: mission_id.to_string(),
            roles: vec![backend_readiness::RoleReadiness {
                role: "worker".into(),
                backend: "claude".into(),
                status: backend_readiness::ReadinessStatus::Missing,
                detail: "no binary".into(),
                next_action: "install".into(),
            }],
            overall: backend_readiness::ReadinessStatus::Missing,
            warnings: vec![],
        }
    }

    fn rate_limited_report(mission_id: &str) -> backend_readiness::ReadinessReport {
        backend_readiness::ReadinessReport {
            mission_id: mission_id.to_string(),
            roles: vec![backend_readiness::RoleReadiness {
                role: "orchestrator".into(),
                backend: "claude".into(),
                status: backend_readiness::ReadinessStatus::RateLimited,
                detail: "429".into(),
                next_action: "wait".into(),
            }],
            overall: backend_readiness::ReadinessStatus::RateLimited,
            warnings: vec![],
        }
    }

    #[tokio::test]
    async fn drain_queue_runs_a_queued_mission_and_retires_its_claim() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        queue::enqueue(
            repo,
            QueueEntry {
                mission_id: "mission-1".to_string(),
                ticket_slug: None,
                priority: 2,
                seq: 0,
            },
        )
        .unwrap();

        let ran = Arc::new(AtomicUsize::new(0));
        let ran_clone = ran.clone();
        let report = drain_queue_with_probe(
            repo,
            false,
            move |mission_id| {
                let ran = ran_clone.clone();
                async move {
                    assert_eq!(mission_id, "mission-1");
                    ran.fetch_add(1, Ordering::SeqCst);
                    Ok(0)
                }
            },
            always_proceed,
        )
        .await
        .unwrap();

        assert_eq!(ran.load(Ordering::SeqCst), 1);
        assert_eq!(report.ran, vec!["mission-1".to_string()]);
        assert!(report.skipped.is_empty());
        // The claim file was retired: nothing left on disk under the queue dir.
        assert!(queue::list(repo).is_empty());
        let dir = queue::queue_dir(repo);
        let leftover: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| {
                e.path()
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.contains(".claimed."))
            })
            .collect();
        assert!(leftover.is_empty(), "claim file was not retired");
    }

    #[tokio::test]
    async fn drain_queue_skips_ticket_whose_blocker_failed() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();

        write_ticket(
            repo,
            "blocker",
            "---\ntitle: blocker\npriority: 2\nschedule: once\n---\n\n## Goal\nblock\n",
        );
        write_ticket(
            repo,
            "dependent",
            "---\ntitle: dependent\npriority: 2\nschedule: once\nblocked-by: [blocker]\n---\n\n## Goal\ndepend\n",
        );
        Ticket::write_state(repo, "blocker", TicketState::Failed, None).unwrap();

        queue::enqueue(
            repo,
            QueueEntry {
                mission_id: "mission-dep".to_string(),
                ticket_slug: Some("dependent".to_string()),
                priority: 2,
                seq: 0,
            },
        )
        .unwrap();

        let ran = Arc::new(AtomicUsize::new(0));
        let ran_clone = ran.clone();
        let report = drain_queue_with_probe(
            repo,
            false,
            move |_mission_id| {
                let ran = ran_clone.clone();
                async move {
                    ran.fetch_add(1, Ordering::SeqCst);
                    Ok(0)
                }
            },
            always_proceed,
        )
        .await
        .unwrap();

        assert_eq!(
            ran.load(Ordering::SeqCst),
            0,
            "the doomed entry must not run"
        );
        assert_eq!(report.skipped, vec!["mission-dep".to_string()]);
        assert!(report.ran.is_empty());
        assert_eq!(Ticket::read_state(repo, "dependent"), TicketState::Failed);
        // The claim was finished, not re-queued (no infinite re-claim).
        assert!(queue::list(repo).is_empty());
    }

    #[tokio::test]
    async fn drain_queue_honors_once() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        for i in 0..2 {
            queue::enqueue(
                repo,
                QueueEntry {
                    mission_id: format!("mission-{i}"),
                    ticket_slug: None,
                    priority: 2,
                    seq: 0,
                },
            )
            .unwrap();
        }

        let ran = Arc::new(AtomicUsize::new(0));
        let ran_clone = ran.clone();
        let report = drain_queue_with_probe(
            repo,
            true,
            move |_mission_id| {
                let ran = ran_clone.clone();
                async move {
                    ran.fetch_add(1, Ordering::SeqCst);
                    Ok(0)
                }
            },
            always_proceed,
        )
        .await
        .unwrap();

        assert_eq!(
            ran.load(Ordering::SeqCst),
            1,
            "--once must run exactly one entry"
        );
        assert_eq!(report.ran.len(), 1);
        // One entry remains queued.
        assert_eq!(queue::list(repo).len(), 1);
    }

    #[tokio::test]
    async fn concurrent_drain_queue_once_reports_busy_without_running_second_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        for i in 1..=2 {
            queue::enqueue(
                repo,
                QueueEntry {
                    mission_id: format!("mission-{i}"),
                    ticket_slug: None,
                    priority: 2,
                    seq: 0,
                },
            )
            .unwrap();
        }

        let first_runs = Arc::new(AtomicUsize::new(0));
        let release_first = Arc::new(tokio::sync::Notify::new());
        let first_repo = repo.to_path_buf();
        let first = {
            let first_runs = first_runs.clone();
            let release_first = release_first.clone();
            tokio::spawn(async move {
                drain_queue_with_probe(
                    &first_repo,
                    true,
                    move |mission_id| {
                        let first_runs = first_runs.clone();
                        let release_first = release_first.clone();
                        async move {
                            assert_eq!(mission_id, "mission-1");
                            first_runs.fetch_add(1, Ordering::SeqCst);
                            release_first.notified().await;
                            Ok(0)
                        }
                    },
                    always_proceed,
                )
                .await
            })
        };

        for _ in 0..50 {
            if first_runs.load(Ordering::SeqCst) == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(
            first_runs.load(Ordering::SeqCst),
            1,
            "first drainer should be holding the repo guard"
        );

        let second_runs = Arc::new(AtomicUsize::new(0));
        let second_runs_clone = second_runs.clone();
        let second = drain_queue_with_probe(
            repo,
            true,
            move |_mission_id| {
                let second_runs = second_runs_clone.clone();
                async move {
                    second_runs.fetch_add(1, Ordering::SeqCst);
                    Ok(0)
                }
            },
            always_proceed,
        )
        .await
        .unwrap();
        assert!(second.stopped_busy);
        assert!(second.ran.is_empty());
        assert_eq!(
            second_runs.load(Ordering::SeqCst),
            0,
            "busy loser must not run the next queued mission"
        );
        assert!(
            queue::contains(repo, "mission-2"),
            "busy loser releases its temporary claim"
        );

        release_first.notify_waiters();
        let first = first.await.unwrap().unwrap();
        assert_eq!(first.ran, vec!["mission-1".to_string()]);
        assert!(queue::contains(repo, "mission-2"));
    }

    #[tokio::test]
    async fn drain_queue_runs_a_ticket_born_entry_to_done() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();

        write_ticket(
            repo,
            "satisfiable",
            "---\ntitle: satisfiable\npriority: 2\nschedule: once\n---\n\n## Goal\nship\n",
        );
        Ticket::record_mission(repo, "satisfiable", "mission-ticket").unwrap();

        queue::enqueue(
            repo,
            QueueEntry {
                mission_id: "mission-ticket".to_string(),
                ticket_slug: Some("satisfiable".to_string()),
                priority: 2,
                seq: 0,
            },
        )
        .unwrap();

        let ran = Arc::new(AtomicUsize::new(0));
        let ran_clone = ran.clone();
        let repo_path = repo.to_path_buf();
        let report = drain_queue_with_probe(
            repo,
            false,
            move |mission_id| {
                let ran = ran_clone.clone();
                let repo_path = repo_path.clone();
                async move {
                    assert_eq!(mission_id, "mission-ticket");
                    ran.fetch_add(1, Ordering::SeqCst);
                    write_events(
                        &repo_path,
                        &mission_id,
                        vec![created(), EventKind::MissionCompleted {}],
                    );
                    Ok(0)
                }
            },
            always_proceed,
        )
        .await
        .unwrap();

        assert_eq!(ran.load(Ordering::SeqCst), 1);
        assert_eq!(report.ran, vec!["mission-ticket".to_string()]);
        assert!(report.skipped.is_empty());
        assert_eq!(Ticket::read_state(repo, "satisfiable"), TicketState::Done);
        // The claim was retired: nothing left queued or claimed on disk.
        assert!(queue::list(repo).is_empty());
        let dir = queue::queue_dir(repo);
        let leftover: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| {
                e.path()
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.contains(".claimed."))
            })
            .collect();
        assert!(leftover.is_empty(), "claim file was not retired");
    }

    /// The mission-id approve path (Slack `/kranz approve m-…`) enqueues a bare
    /// entry with `ticket_slug: None`; drain must resolve the linked ticket via
    /// the reverse lookup so its pipeline state still advances to Done. Pins the
    /// `.or_else(Ticket::slug_for_mission)` fallback — without it, a bare entry
    /// runs but the ticket is left stuck in Review.
    #[tokio::test]
    async fn drain_queue_resolves_a_bare_entry_to_its_linked_ticket() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();

        write_ticket(
            repo,
            "linked",
            "---\ntitle: linked\npriority: 2\nschedule: once\n---\n\n## Goal\nship\n",
        );
        // Link the ticket to the mission and park it mid-pipeline, exactly as
        // the Slack approve-by-mission-id flow leaves it.
        Ticket::record_mission(repo, "linked", "m-linked").unwrap();
        Ticket::write_state(repo, "linked", TicketState::Queued, None).unwrap();

        // A sibling ticket linked to a DIFFERENT mission must not be resolved.
        write_ticket(
            repo,
            "other",
            "---\ntitle: other\npriority: 2\nschedule: once\n---\n\n## Goal\nnope\n",
        );
        Ticket::record_mission(repo, "other", "m-other").unwrap();

        queue::enqueue(
            repo,
            QueueEntry {
                mission_id: "m-linked".to_string(),
                ticket_slug: None, // bare: the fallback must find "linked"
                priority: 2,
                seq: 0,
            },
        )
        .unwrap();

        let repo_path = repo.to_path_buf();
        let report = drain_queue_with_probe(
            repo,
            false,
            move |mission_id| {
                let repo_path = repo_path.clone();
                async move {
                    assert_eq!(mission_id, "m-linked");
                    write_events(
                        &repo_path,
                        &mission_id,
                        vec![created(), EventKind::MissionCompleted {}],
                    );
                    Ok(0)
                }
            },
            always_proceed,
        )
        .await
        .unwrap();

        assert_eq!(report.ran, vec!["m-linked".to_string()]);
        assert_eq!(
            Ticket::read_state(repo, "linked"),
            TicketState::Done,
            "the linked ticket must advance via the reverse lookup"
        );
        assert_eq!(
            Ticket::read_state(repo, "other"),
            TicketState::Drafting,
            "an unrelated ticket must be untouched (record_mission left it Drafting)"
        );
        assert!(queue::list(repo).is_empty());
    }

    #[tokio::test]
    async fn drain_queue_parks_ticket_when_readiness_fails_after_claim() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();

        write_ticket(
            repo,
            "need-auth",
            "---\ntitle: need-auth\npriority: 2\nschedule: once\n---\n\n## Goal\nship\n",
        );
        queue::enqueue(
            repo,
            QueueEntry {
                mission_id: "m-park".to_string(),
                ticket_slug: Some("need-auth".to_string()),
                priority: 2,
                seq: 0,
            },
        )
        .unwrap();

        let ran = Arc::new(AtomicUsize::new(0));
        let ran_clone = ran.clone();
        let report = drain_queue_with_probe(
            repo,
            false,
            move |_mission_id| {
                let ran = ran_clone.clone();
                async move {
                    ran.fetch_add(1, Ordering::SeqCst);
                    Ok(0)
                }
            },
            |_repo, id| Ok(park_report(id)),
        )
        .await
        .unwrap();

        assert_eq!(ran.load(Ordering::SeqCst), 0);
        assert_eq!(report.parked, vec!["m-park".to_string()]);
        assert!(report.ran.is_empty());
        assert_eq!(Ticket::read_state(repo, "need-auth"), TicketState::Parked);
        assert!(queue::list(repo).is_empty());
    }

    #[tokio::test]
    async fn drain_queue_rate_limit_rotates_then_parks_after_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();

        write_ticket(
            repo,
            "limited",
            "---\ntitle: limited\npriority: 2\nschedule: once\n---\n\n## Goal\na\n",
        );
        write_ticket(
            repo,
            "sibling",
            "---\ntitle: sibling\npriority: 2\nschedule: once\n---\n\n## Goal\nb\n",
        );
        Ticket::record_mission(repo, "sibling", "m-sibling").unwrap();
        queue::enqueue(
            repo,
            QueueEntry {
                mission_id: "m-limited".to_string(),
                ticket_slug: Some("limited".to_string()),
                priority: 2,
                seq: 0,
            },
        )
        .unwrap();
        queue::enqueue(
            repo,
            QueueEntry {
                mission_id: "m-sibling".to_string(),
                ticket_slug: Some("sibling".to_string()),
                priority: 2,
                seq: 0,
            },
        )
        .unwrap();

        // Rate-limit sleep is clamped under cfg(test) in drain_queue_with_probe.
        let probe = |_repo: &Path, id: &str| {
            if id == "m-limited" {
                Ok(rate_limited_report(id))
            } else {
                Ok(proceed_report(id))
            }
        };

        let ran = Arc::new(AtomicUsize::new(0));
        let ran_clone = ran.clone();
        let repo_path = repo.to_path_buf();
        let report = drain_queue_with_probe(
            repo,
            false,
            move |mission_id| {
                let ran = ran_clone.clone();
                let repo_path = repo_path.clone();
                async move {
                    assert_eq!(mission_id, "m-sibling");
                    ran.fetch_add(1, Ordering::SeqCst);
                    write_events(
                        &repo_path,
                        &mission_id,
                        vec![created(), EventKind::MissionCompleted {}],
                    );
                    Ok(0)
                }
            },
            probe,
        )
        .await
        .unwrap();

        assert_eq!(ran.load(Ordering::SeqCst), 1);
        assert_eq!(report.ran, vec!["m-sibling".to_string()]);
        assert_eq!(report.parked, vec!["m-limited".to_string()]);
        assert_eq!(Ticket::read_state(repo, "sibling"), TicketState::Done);
        assert_eq!(Ticket::read_state(repo, "limited"), TicketState::Parked);
        assert!(queue::list(repo).is_empty());
    }

    /// Pins the drain loop's terminal ticket write to `reconcile_ticket_for_mission`
    /// (not a local exit-code mapping): a mission that folds to Complete leaves
    /// its ticket Done, while one that folds to Blocked leaves it NeedsContext —
    /// never Failed, even though `run_mission` still returns `Ok(0)` in both cases.
    #[tokio::test]
    async fn drain_reconciles_terminal_ticket_via_helper() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();

        write_ticket(
            repo,
            "done-ticket",
            "---\ntitle: done\npriority: 2\nschedule: once\n---\n\n## Goal\nship\n",
        );
        write_ticket(
            repo,
            "blocked-ticket",
            "---\ntitle: blocked\npriority: 2\nschedule: once\n---\n\n## Goal\nship\n",
        );
        Ticket::record_mission(repo, "done-ticket", "m-done").unwrap();
        Ticket::record_mission(repo, "blocked-ticket", "m-blocked").unwrap();

        queue::enqueue(
            repo,
            QueueEntry {
                mission_id: "m-done".to_string(),
                ticket_slug: Some("done-ticket".to_string()),
                priority: 2,
                seq: 0,
            },
        )
        .unwrap();
        queue::enqueue(
            repo,
            QueueEntry {
                mission_id: "m-blocked".to_string(),
                ticket_slug: Some("blocked-ticket".to_string()),
                priority: 2,
                seq: 1,
            },
        )
        .unwrap();

        let repo_path = repo.to_path_buf();
        let report = drain_queue_with_probe(
            repo,
            false,
            move |mission_id| {
                let repo_path = repo_path.clone();
                async move {
                    if mission_id == "m-done" {
                        write_events(
                            &repo_path,
                            &mission_id,
                            vec![created(), EventKind::MissionCompleted {}],
                        );
                    } else {
                        write_events(
                            &repo_path,
                            &mission_id,
                            vec![
                                created(),
                                EventKind::PlanApproved {
                                    plan: plan_with_one_milestone(),
                                    base_sha: None,
                                },
                                EventKind::MilestoneBlocked {
                                    milestone_id: "ms-1".to_string(),
                                    reason: "needs input".to_string(),
                                },
                            ],
                        );
                    }
                    // Both missions return Ok(0): the exit code must not
                    // determine the ticket's terminal state any more.
                    Ok(0)
                }
            },
            always_proceed,
        )
        .await
        .unwrap();

        assert_eq!(report.ran.len(), 2);
        assert_eq!(Ticket::read_state(repo, "done-ticket"), TicketState::Done);
        assert_eq!(
            Ticket::read_state(repo, "blocked-ticket"),
            TicketState::NeedsContext
        );
    }
}

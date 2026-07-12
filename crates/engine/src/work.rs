//! Host-callable queue drain core (roadmap f-1-1): the drain/claim/skip loop
//! that used to live only inside `kranz` CLI's `cmd_work`, hoisted here so any
//! surface (CLI, REST, Slack) can drain a repo's queue.
//!
//! [`drain_queue`] owns the loop — recover dead claims, then peek/claim/run
//! one mission at a time — but does NOT run missions itself: the caller
//! injects a `run_mission` closure, because each host supplies its own runner
//! (the CLI tails events to stderr; a headless caller does not) and only the
//! caller knows how to restore its own git checkout.

use crate::deps;
use crate::queue::{self, QueueEntry};
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

/// Map a terminal mission status to the ticket state recorded after a run.
pub fn ticket_state_for_mission(status: MissionStatus) -> TicketState {
    match status {
        MissionStatus::Complete => TicketState::Done,
        // Blocked/Failed/anything-non-complete leaves the ticket Failed so it
        // resurfaces in `ticket list` for a human to pick back up.
        _ => TicketState::Failed,
    }
}

/// Map a `run_mission` exit code to the ticket's terminal state (0 → Done,
/// anything else → Failed, matching [`ticket_state_for_mission`]).
fn mission_state_from_code(code: i32) -> TicketState {
    if code == 0 {
        TicketState::Done
    } else {
        TicketState::Failed
    }
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
    /// `once` stopped the drain while the repo was busy with another mission
    /// (nothing here was claimed or run). Callers that restore their own git
    /// checkout on exit must NOT do so when this is set — the repo is still
    /// mid-mission under a sibling dispatcher, and switching branches out
    /// from under it would corrupt that run's working tree.
    pub stopped_busy: bool,
}

/// Drain the per-repo queue one mission at a time. Recover dead claims once up
/// front; then atomically claim the front entry with the repo-wide busy guard;
/// on `Busy` either return (`once`) or sleep 5s and retry; on a lost claim race,
/// retry after a brief sleep; on `Claimed`, take the CLAIMED entry's mission id
/// as authoritative, skip a ticket-born entry whose blocker failed (finish the
/// claim + write ticket Failed), otherwise write ticket Running, invoke the injected
/// `run_mission`, then finish/release the claim (finish on success, finish on
/// a ticket-born error so it doesn't re-claim forever, release on a bare-entry
/// error so the work isn't lost), write the terminal ticket state, and honor
/// `once`.
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
    let mut report = DrainReport::default();
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
                // Mission-id approve paths (e.g. Slack `/kranz approve m-…`)
                // historically enqueued with `ticket_slug: None`. Resolve the
                // linked ticket so Running/Done still advance.
                let ticket_slug = claim
                    .entry
                    .ticket_slug
                    .clone()
                    .or_else(|| Ticket::slug_for_mission(repo_root, &mission_id));
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

                // Terminal outcome (any) retires the claim. A mission we
                // could not RUN (lock held, config, spawn failure): for a
                // ticket-born entry the ticket is marked Failed below and the
                // claim retires with it (no re-run loop); a bare entry is
                // RELEASED so the work isn't lost, and the `status?` below
                // stops this dispatcher rather than hot-looping.
                match &status {
                    Ok(_) => queue::finish_claim(claim),
                    Err(_) if ticket_slug.is_some() => queue::finish_claim(claim),
                    Err(_) => queue::release_claim(claim),
                }

                if let Some(slug) = &ticket_slug {
                    match &status {
                        Ok(code) => {
                            Ticket::write_state(
                                repo_root,
                                slug,
                                mission_state_from_code(*code),
                                None,
                            )?;
                        }
                        Err(_) => {
                            Ticket::write_state(repo_root, slug, TicketState::Failed, None)?;
                        }
                    }
                } else {
                    status?;
                }
                report.ran.push(mission_id);

                if once {
                    return Ok(report);
                }
                // Loop: re-check the queue for the next mission.
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
            TicketState::Failed
        );
        assert_eq!(
            ticket_state_for_mission(MissionStatus::Failed),
            TicketState::Failed
        );
    }

    fn write_ticket(repo: &Path, slug: &str, body: &str) {
        let dir = Ticket::tickets_dir(repo);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{slug}.md")), body).unwrap();
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
        let report = drain_queue(repo, false, move |mission_id| {
            let ran = ran_clone.clone();
            async move {
                assert_eq!(mission_id, "mission-1");
                ran.fetch_add(1, Ordering::SeqCst);
                Ok(0)
            }
        })
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
        let report = drain_queue(repo, false, move |_mission_id| {
            let ran = ran_clone.clone();
            async move {
                ran.fetch_add(1, Ordering::SeqCst);
                Ok(0)
            }
        })
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
        let report = drain_queue(repo, true, move |_mission_id| {
            let ran = ran_clone.clone();
            async move {
                ran.fetch_add(1, Ordering::SeqCst);
                Ok(0)
            }
        })
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
                drain_queue(&first_repo, true, move |mission_id| {
                    let first_runs = first_runs.clone();
                    let release_first = release_first.clone();
                    async move {
                        assert_eq!(mission_id, "mission-1");
                        first_runs.fetch_add(1, Ordering::SeqCst);
                        release_first.notified().await;
                        Ok(0)
                    }
                })
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
        let second = drain_queue(repo, true, move |_mission_id| {
            let second_runs = second_runs_clone.clone();
            async move {
                second_runs.fetch_add(1, Ordering::SeqCst);
                Ok(0)
            }
        })
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
        let report = drain_queue(repo, false, move |mission_id| {
            let ran = ran_clone.clone();
            async move {
                assert_eq!(mission_id, "mission-ticket");
                ran.fetch_add(1, Ordering::SeqCst);
                Ok(0)
            }
        })
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

        let report = drain_queue(repo, false, move |mission_id| async move {
            assert_eq!(mission_id, "m-linked");
            Ok(0)
        })
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
}

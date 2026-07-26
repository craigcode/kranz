//! Non-interactive draft core (roadmap f-1-1): the sequencing behind
//! `kranz draft`, hoisted out of the CLI so any surface (CLI, REST) can drive
//! a ticket through its planning conversation.
//!
//! [`drive_draft`] owns the engine call order — `write_state(Drafting)` →
//! `record_mission` → `planning_turn` (seeded with the whole ticket) →
//! `request_plan` → branch on the result — and the terminal filesystem/queue
//! side effects ([`Ticket::append_needs_context`],
//! [`Ticket::append_wrong_plan`], `approve_plan`, `queue::enqueue`). It does
//! not print anything and does not touch the operator's git checkout; both
//! stay with the caller.

use crate::error::Result;
use crate::orchestrator::{MissionEngine, PlanRequest};
use crate::queue::{self, QueueEntry};
use crate::ticket::{Ticket, TicketState};
use crate::types::Plan;
use std::path::Path;

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
    Approve {
        then_enqueue: bool,
        next_state: TicketState,
    },
    /// Orchestrator wants answers first: append its questions to the ticket
    /// and set `NeedsContext`. Short-circuits before any approval.
    NeedsContext { questions: Vec<String> },
    /// Planner-initiated escalation: it CAN plan but believes the plan is
    /// likely wrong. Append the reason to the ticket and set `WrongPlan`.
    /// Short-circuits before any approval; `--yes` never overrides it.
    WrongPlan { reason: String },
}

/// Map a completed plan request + the `--yes` flag to the next action. Pure:
/// the caller performs the git/state side effects the decision names.
pub fn draft_decision(request: &PlanRequest, yes: bool) -> DraftDecision {
    match request {
        PlanRequest::Ready(_) => DraftDecision::Approve {
            then_enqueue: yes,
            next_state: if yes {
                TicketState::Queued
            } else {
                TicketState::Review
            },
        },
        PlanRequest::NotReady(text) => DraftDecision::NeedsContext {
            questions: split_questions(text),
        },
        PlanRequest::WrongPlan { reason } => DraftDecision::WrongPlan {
            reason: reason.clone(),
        },
    }
}

/// Does a NotReady reply look like a COMPLETE plan the orchestrator chatted
/// out as prose instead of returning through the plan channel? Matches the
/// plan schema's two distinctive top-level keys.
pub fn looks_like_plan_json(reply: &str) -> bool {
    reply.contains("\"validationContract\"") && reply.contains("\"milestones\"")
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
// drive_draft — the sequencing core
// ---------------------------------------------------------------------------

/// Terminal result of driving one ticket through a draft turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DraftOutcome {
    /// Plan approved and committed on `mission_branch`, parked for review
    /// (ticket set to [`TicketState::Review`]).
    ParkedForReview {
        mission_id: String,
        mission_branch: String,
    },
    /// Plan approved and the mission enqueued (ticket set to
    /// [`TicketState::Queued`]).
    Enqueued { mission_id: String },
    /// The orchestrator wants answers first (ticket set to
    /// [`TicketState::NeedsContext`], questions appended to the ticket body).
    NeedsContext {
        mission_id: String,
        questions: Vec<String>,
    },
    /// The planner escalated: it can produce a plan but believes it is
    /// likely wrong (ticket set to [`TicketState::WrongPlan`], reason
    /// appended to the ticket body, `.status` note prefixed `WRONG-PLAN: `).
    WrongPlan { mission_id: String, reason: String },
    /// The orchestrator produced a plan but emitted it as prose instead of
    /// through the plan channel, and a bounded retry did not recover it. No
    /// plan JSON is filed to the ticket body; the ticket is parked in
    /// NeedsContext with a short .status note and the user re-runs draft.
    PlanAsProse { mission_id: String },
}

/// [`drive_draft`]'s return: the terminal [`DraftOutcome`] plus the display
/// payload a caller needs to reproduce the pre-hoist CLI output exactly —
/// the seed reply (from the session-start turn) and, on the Approve path,
/// the approved [`Plan`]. The core itself never prints either; it only
/// avoids dropping them.
#[derive(Debug, Clone)]
pub struct DraftDrive {
    pub outcome: DraftOutcome,
    /// The orchestrator's session-start reply, captured before `request_plan`.
    pub seed_reply: Option<String>,
    /// The approved plan, cloned before it was moved into `approve_plan`.
    /// `None` on the `NeedsContext`, `WrongPlan`, and `PlanAsProse` paths.
    pub plan: Option<Plan>,
}

/// Drive `ticket` through one non-interactive draft turn against an
/// already-constructed `engine` (holding its backend): seed the orchestrator
/// with the whole ticket, demand the plan, and resolve via [`draft_decision`].
///
/// Backend-agnostic and side-effect-scoped to the ticket/queue filesystem
/// state — no printing, no checkout restoration (the caller's job). On a seed
/// or plan-request error the ticket is rolled back to [`TicketState::New`]
/// and the error is propagated.
pub async fn drive_draft(
    engine: &mut MissionEngine,
    repo: &Path,
    ticket: &Ticket,
    then_enqueue: bool,
) -> Result<DraftDrive> {
    let slug = ticket.slug.as_str();
    Ticket::write_state(repo, slug, TicketState::Drafting, None)?;

    let mission_id = engine.mission_id().to_string();
    Ticket::record_mission(repo, slug, &mission_id)?;

    let goal = ticket.mission_goal();
    if let Err(e) = engine.planning_turn(&goal).await {
        Ticket::write_state(repo, slug, TicketState::New, None)?;
        return Err(e);
    }
    // Capture the seed reply (session-start turn) so the caller can display
    // it exactly as pre-hoist `cmd_draft` did; the core itself never prints.
    let seed_reply = engine.take_seed_reply();

    let request = match engine.request_plan().await {
        Ok(r) => r,
        Err(e) => {
            Ticket::write_state(repo, slug, TicketState::New, None)?;
            return Err(e);
        }
    };

    // A NotReady reply that reads as a complete plan JSON blob means the
    // orchestrator chatted the plan out instead of returning through the plan
    // channel — filing that blob as "questions" would dump multi-KB plan JSON
    // into the ticket body. Give it exactly one more chance via the plan
    // channel before giving up honestly.
    if let PlanRequest::NotReady(text) = &request {
        if looks_like_plan_json(text) {
            return match engine.request_plan().await {
                Ok(PlanRequest::Ready(plan)) => Ok(approve(
                    engine,
                    repo,
                    slug,
                    &mission_id,
                    ticket,
                    plan,
                    then_enqueue,
                    seed_reply,
                )?),
                _ => {
                    Ticket::write_state(
                        repo,
                        slug,
                        TicketState::NeedsContext,
                        Some(
                            "The orchestrator produced a plan but emitted it as prose instead \
                             of through the plan channel — re-run `kranz draft` for this ticket."
                                .to_string(),
                        ),
                    )?;
                    Ok(DraftDrive {
                        outcome: DraftOutcome::PlanAsProse { mission_id },
                        seed_reply,
                        plan: None,
                    })
                }
            };
        }
    }

    match draft_decision(&request, then_enqueue) {
        DraftDecision::NeedsContext { questions } => {
            Ticket::append_needs_context(repo, slug, &questions)?;
            Ok(DraftDrive {
                outcome: DraftOutcome::NeedsContext {
                    mission_id,
                    questions,
                },
                seed_reply,
                plan: None,
            })
        }
        DraftDecision::WrongPlan { reason } => {
            Ticket::append_wrong_plan(repo, slug, &reason)?;
            Ok(DraftDrive {
                outcome: DraftOutcome::WrongPlan { mission_id, reason },
                seed_reply,
                plan: None,
            })
        }
        DraftDecision::Approve {
            then_enqueue,
            next_state: _,
        } => {
            let PlanRequest::Ready(plan) = request else {
                unreachable!("Approve decision implies a Ready plan");
            };
            approve(
                engine,
                repo,
                slug,
                &mission_id,
                ticket,
                plan,
                then_enqueue,
                seed_reply,
            )
        }
    }
}

/// Shared approval side effects for a [`PlanRequest::Ready`] plan, whether it
/// arrived via the normal path or the plan-as-prose bounded retry: commit the
/// plan (`approve_plan`), then either enqueue+park `Queued` or park `Review`.
#[allow(clippy::too_many_arguments)]
fn approve(
    engine: &mut MissionEngine,
    repo: &Path,
    slug: &str,
    mission_id: &str,
    ticket: &Ticket,
    plan: Plan,
    then_enqueue: bool,
    seed_reply: Option<String>,
) -> Result<DraftDrive> {
    let approved_plan = plan.clone();
    engine.approve_plan(plan)?;
    let mission_branch = engine.state().mission.mission_branch.clone();

    if then_enqueue {
        queue::enqueue(
            repo,
            QueueEntry {
                mission_id: mission_id.to_string(),
                ticket_slug: Some(slug.to_string()),
                priority: ticket.priority,
                seq: 0, // assigned by enqueue
            },
        )?;
        Ticket::write_state(repo, slug, TicketState::Queued, None)?;
        Ok(DraftDrive {
            outcome: DraftOutcome::Enqueued {
                mission_id: mission_id.to_string(),
            },
            seed_reply,
            plan: Some(approved_plan),
        })
    } else {
        Ticket::write_state(repo, slug, TicketState::Review, None)?;
        Ok(DraftDrive {
            outcome: DraftOutcome::ParkedForReview {
                mission_id: mission_id.to_string(),
                mission_branch,
            },
            seed_reply,
            plan: Some(approved_plan),
        })
    }
}

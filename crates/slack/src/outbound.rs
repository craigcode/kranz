//! Pure classification of mission events into outbound Slack notifications.
//!
//! The bridge tails each mission's `events.jsonl`, folds state incrementally
//! (exactly as the server's WS does), and for each new event asks
//! [`classify`] whether it warrants a Slack post. Keeping the "which event →
//! which message" decision here — as a pure function of the event + the state
//! *after* applying it — makes the whole notification policy unit-testable
//! against synthetic states, with no tailing loop or socket in sight.
//!
//! Event → notification map (gated by [`crate::config::NotifyFlags`] in the bridge):
//! - `plan.approved`      → plan ready for review (`build_plan_ready`)
//! - `plan.revision.proposed` → revision ready for review (`build_revision_ready`)
//! - `milestone.blocked`  → blocked (`build_blocked`)
//! - `mission.completed`  → complete (`build_complete`, [`Outcome::Completed`])
//! - `mission.failed`     → complete (`build_complete`, [`Outcome::Failed`])
//!
//! Needs-context is NOT an engine event (the draft driver writes it to the
//! ticket file), so the bridge emits that one directly from the ticket layer;
//! see [`crate::format::build_needs_context`]. It is intentionally absent here.

use crate::format::{
    Blocked, Complete, GrantReady, Outcome, PlanReady, QuestionReady, RevisionReady,
};
use kranz_engine::events::{Event, EventKind};
use kranz_engine::types::MissionState;
use std::path::Path;

/// A notification to post, tagged by class so the bridge can gate on
/// [`crate::config::NotifyFlags`] and pick the formatter.
#[derive(Debug, Clone)]
pub enum Outbound {
    PlanReady(PlanReady),
    RevisionReady(RevisionReady),
    GrantReady(GrantReady),
    QuestionReady(QuestionReady),
    Blocked(Blocked),
    Complete(Complete),
}

impl Outbound {
    /// The notify class, so the bridge can consult the flags without matching
    /// on the payload variant.
    pub fn class(&self) -> NotifyClass {
        match self {
            Outbound::PlanReady(_) => NotifyClass::PlanReady,
            Outbound::RevisionReady(_) => NotifyClass::PlanReady,
            // A parked grant is an attention-needed block on the milestone, so
            // it rides the same notify flag as `milestone.blocked`.
            Outbound::GrantReady(_) => NotifyClass::Blocked,
            // An open question is the SAME attention-needed "your move" as a
            // parked grant (one shared pending-decision area), so it rides
            // the same flag.
            Outbound::QuestionReady(_) => NotifyClass::Blocked,
            Outbound::Blocked(_) => NotifyClass::Blocked,
            Outbound::Complete(_) => NotifyClass::Complete,
        }
    }
}

/// The notify classes the outbound classifier can produce. (Needs-context is
/// handled off the event stream, so it is not here.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyClass {
    PlanReady,
    Blocked,
    Complete,
}

/// Classify one event, given the mission state *after* applying it, into an
/// optional outbound notification. Returns `None` for the many events that
/// don't warrant a Slack post (worker deltas, feature transitions, …).
///
/// `repo_root` is only consulted for `MissionCompleted` (to source the
/// Delivered card's diff stat, plan §M3 "Merge on both human surfaces"); every
/// other branch stays a pure function of `event` + `state`. Git access is
/// best-effort — any failure (no repo, unpinned base, missing branch) just
/// degrades `diff_stat` to `None`, same as an unset cost.
pub fn classify(event: &Event, state: &MissionState, repo_root: &Path) -> Option<Outbound> {
    match &event.kind {
        EventKind::PlanApproved { plan, .. } => Some(Outbound::PlanReady(PlanReady {
            mission_id: state.mission.id.clone(),
            goal: plan.goal.clone(),
            milestone_titles: plan.milestones.iter().map(|m| m.title.clone()).collect(),
            assertion_count: plan.validation_contract.len(),
            plan_identity: crate::format::plan_identity(plan),
        })),

        EventKind::PlanRevisionProposed {
            revision,
            plan,
            instructions,
        } => Some(Outbound::RevisionReady(RevisionReady {
            mission_id: state.mission.id.clone(),
            revision: *revision,
            instructions: instructions.clone(),
            milestone_titles: plan.milestones.iter().map(|m| m.title.clone()).collect(),
            assertion_count: plan.validation_contract.len(),
        })),

        EventKind::GrantRequested {
            milestone_id,
            kind,
            command,
        } => Some(Outbound::GrantReady(GrantReady {
            mission_id: state.mission.id.clone(),
            milestone_id: milestone_id.clone(),
            kind: *kind,
            command: command.clone(),
        })),

        // An opened question is the second kind of the shared "your move"
        // area; its answer/clear events produce no card (mirroring grant
        // decisions, which also don't notify).
        EventKind::QuestionOpened {
            question_id,
            text,
            options,
            feature_id,
            milestone_id,
            ..
        } => Some(Outbound::QuestionReady(QuestionReady {
            mission_id: state.mission.id.clone(),
            question_id: question_id.clone(),
            text: text.clone(),
            options: options.clone(),
            feature_id: feature_id.clone(),
            milestone_id: milestone_id.clone(),
        })),

        EventKind::MilestoneBlocked {
            milestone_id,
            reason,
        } => Some(Outbound::Blocked(Blocked {
            mission_id: state.mission.id.clone(),
            milestone_id: milestone_id.clone(),
            reason: reason.clone(),
        })),

        EventKind::MissionCompleted {} => Some(Outbound::Complete(Complete {
            mission_id: state.mission.id.clone(),
            outcome: Outcome::Completed,
            summary: completion_summary(state),
            branch: state.mission.mission_branch.clone(),
            cost_usd: nonzero(state.total_cost_usd),
            diff_stat: mission_diff_stat(repo_root, &state.mission),
            pr_handoff: mission_pr_handoff_hint(repo_root, &state.mission),
        })),

        EventKind::MissionFailed { reason } => Some(Outbound::Complete(Complete {
            mission_id: state.mission.id.clone(),
            outcome: Outcome::Failed,
            summary: reason.clone(),
            branch: state.mission.mission_branch.clone(),
            cost_usd: nonzero(state.total_cost_usd),
            diff_stat: None,
            pr_handoff: None,
        })),

        _ => None,
    }
}

/// `git diff --stat` of `base_sha..mission_branch`, best-effort. `None` when
/// the base isn't pinned yet, the repo can't be opened, the mission branch
/// doesn't exist, or any git step fails — mirrors the tolerance of
/// `crates/server`'s `mission_diff_stat` REST handler, which sources the same
/// value for the dashboard's Delivered panel.
fn mission_diff_stat(repo_root: &Path, mission: &kranz_engine::types::Mission) -> Option<String> {
    let base_sha = mission.base_sha.as_deref()?;
    let repo = kranz_engine::git_ops::GitRepo::open(repo_root).ok()?;
    if !repo.branch_exists(&mission.mission_branch).ok()? {
        return None;
    }
    let tip = repo.rev_parse(&mission.mission_branch).ok()?;
    repo.diff_stat(base_sha, &tip).ok()
}

/// Copyable PR handoff hint for Slack (push command or gh create). Never
/// performs a push — assessment only.
fn mission_pr_handoff_hint(
    repo_root: &Path,
    mission: &kranz_engine::types::Mission,
) -> Option<String> {
    use kranz_engine::pr_handoff::{assess, PrHandoff, PrHandoffInputs};
    // Never probe the remote from Slack notify — ls-remote can hang the bridge.
    let handoff = assess(
        repo_root,
        &PrHandoffInputs {
            mission,
            report_md: None,
            plan_md: None,
            remote: "origin",
            probe_remote: false,
        },
    );
    match handoff {
        PrHandoff::NeedsPush { command, .. } => Some(command),
        PrHandoff::ReadyToCreate { command, .. } => Some(command),
        PrHandoff::Unavailable { reason } => Some(reason),
    }
}

/// A one-line completion summary from the folded state: the mission goal plus a
/// milestone tally. `report.md` is the full artifact (linked by branch); this
/// is just the Slack teaser.
fn completion_summary(state: &MissionState) -> String {
    let total = state.mission.milestones.len();
    let done = state
        .mission
        .milestones
        .iter()
        .filter(|m| matches!(m.status, kranz_engine::types::MilestoneStatus::Complete))
        .count();
    let goal = state.mission.goal.trim();
    if goal.is_empty() {
        format!("{done}/{total} milestones complete.")
    } else {
        format!("{goal}\n\n{done}/{total} milestones complete.")
    }
}

/// Map a zero cost to `None` (unknown/free) so the formatter omits the line.
fn nonzero(cost: f64) -> Option<f64> {
    (cost > 0.0).then_some(cost)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use kranz_engine::events::EventKind;
    use kranz_engine::types::*;

    fn base_state() -> MissionState {
        MissionState {
            feature_base_shas: Default::default(),
            mission: Mission {
                id: "m-1".into(),
                goal: "Rate-limit the notes API".into(),
                validation_contract: vec![],
                milestones: vec![Milestone {
                    id: "ms-1".into(),
                    title: "Token bucket".into(),
                    features: vec![],
                    status: MilestoneStatus::Complete,
                    fix_cycles: 0,
                    start_sha: None,
                    validator_guidance: None,
                }],
                status: MissionStatus::Running,
                created_at: Utc::now(),
                base_branch: "main".into(),
                base_sha: None,
                mission_branch: "kranz/mission-m-1".into(),
                command_grants: vec![],
                touch_set: vec![],
                deny_exceptions: vec![],
                egress_grants: vec![],
                executor_route: None,
                standards_manifest: None,
            },
            runs: Default::default(),
            totals: TokenUsage::default(),
            total_cost_usd: 3.5,
            pending_user_messages: vec![],
            recent_decisions: vec![],
            config: MissionConfig::default(),
            latest_plan_revision: 0,
            pending_revision: None,
            pending_grant_request: None,
            pending_questions: vec![],
            question_count: 0,
            last_seq: 1,
            escalated_milestones: 0,
            local_executor_milestones: 0,
            workspace_provider: None,
            workspace_pin: None,
            workspace_lifecycle: None,
            resolved_divergence_units: Default::default(),
        }
    }

    fn ev(kind: EventKind) -> Event {
        Event {
            seq: 1,
            ts: Utc::now(),
            mission_id: "m-1".into(),
            kind,
        }
    }

    fn no_repo() -> &'static Path {
        Path::new("/nonexistent/kranz-test-repo-root")
    }

    #[test]
    fn plan_approved_classifies_plan_ready() {
        let plan = Plan {
            goal: "Rate-limit the notes API".into(),
            validation_contract: vec![Assertion {
                id: "a1".into(),
                statement: "429 beyond N/min".into(),
                check: AssertionCheck::Command,
                command: Some("pytest".into()),
                pty_script: None,
            }],
            milestones: vec![PlanMilestone {
                title: "Token bucket".into(),
                features: vec![],
            }],
            considered_alternatives: None,
            command_grants: vec![],
            touch_set: vec![],
            standards_manifest: None,
        };
        let out = classify(
            &ev(EventKind::PlanApproved {
                plan,
                base_sha: None,
            }),
            &base_state(),
            no_repo(),
        )
        .unwrap();
        assert_eq!(out.class(), NotifyClass::PlanReady);
        let Outbound::PlanReady(p) = out else {
            panic!("wrong variant")
        };
        assert_eq!(p.mission_id, "m-1");
        assert_eq!(p.milestone_titles, vec!["Token bucket".to_string()]);
        assert_eq!(p.assertion_count, 1);
    }

    #[test]
    fn plan_revision_proposed_classifies_revision_ready() {
        let plan = Plan {
            goal: "Rate-limit the notes API".into(),
            validation_contract: vec![Assertion {
                id: "a1".into(),
                statement: "429 beyond N/min".into(),
                check: AssertionCheck::Command,
                command: Some("pytest".into()),
                pty_script: None,
            }],
            milestones: vec![PlanMilestone {
                title: "Safer bucket".into(),
                features: vec![],
            }],
            considered_alternatives: None,
            command_grants: vec![],
            touch_set: vec![],
            standards_manifest: None,
        };
        let out = classify(
            &ev(EventKind::PlanRevisionProposed {
                revision: 4,
                plan,
                instructions: "reduce scope".into(),
            }),
            &base_state(),
            no_repo(),
        )
        .unwrap();
        assert_eq!(out.class(), NotifyClass::PlanReady);
        let Outbound::RevisionReady(r) = out else {
            panic!("wrong variant")
        };
        assert_eq!(r.mission_id, "m-1");
        assert_eq!(r.revision, 4);
        assert_eq!(r.instructions, "reduce scope");
        assert_eq!(r.milestone_titles, vec!["Safer bucket".to_string()]);
        assert_eq!(r.assertion_count, 1);
    }

    #[test]
    fn grant_requested_classifies_grant_ready() {
        let out = classify(
            &ev(EventKind::GrantRequested {
                milestone_id: "ms-1".into(),
                kind: kranz_engine::types::GrantKind::Command,
                command: "gc audit --deep".into(),
            }),
            &base_state(),
            no_repo(),
        )
        .unwrap();
        // A parked grant rides the Blocked notify flag (attention-needed).
        assert_eq!(out.class(), NotifyClass::Blocked);
        let Outbound::GrantReady(g) = out else {
            panic!("wrong variant")
        };
        assert_eq!(g.mission_id, "m-1");
        assert_eq!(g.milestone_id, "ms-1");
        assert_eq!(g.command, "gc audit --deep");
    }

    /// The question card (ticket structured-human-question-events): an opened
    /// question classifies into the SAME attention-needed notify class as a
    /// parked grant (the shared "your move" flag); answers and clears produce
    /// no card, mirroring grant decisions.
    #[test]
    fn question_events_opened_classifies_question_ready() {
        let out = classify(
            &ev(EventKind::QuestionOpened {
                question_id: "q-1".into(),
                role: kranz_engine::types::Role::Worker,
                text: "Which storage engine?".into(),
                options: vec!["sqlite".into(), "in-memory".into()],
                run_id: Some("r-1".into()),
                feature_id: Some("f-1-1".into()),
                milestone_id: Some("ms-1".into()),
            }),
            &base_state(),
            no_repo(),
        )
        .unwrap();
        assert_eq!(out.class(), NotifyClass::Blocked);
        let Outbound::QuestionReady(q) = out else {
            panic!("wrong variant")
        };
        assert_eq!(q.mission_id, "m-1");
        assert_eq!(q.question_id, "q-1");
        assert_eq!(q.text, "Which storage engine?");
        assert_eq!(q.options, vec!["sqlite", "in-memory"]);
        assert_eq!(q.feature_id.as_deref(), Some("f-1-1"));
        assert_eq!(q.milestone_id.as_deref(), Some("ms-1"));

        // Answers and clears notify nothing (mirroring grant decisions).
        for kind in [
            EventKind::QuestionAnswered {
                question_id: "q-1".into(),
                answer: "sqlite".into(),
                via: "answer-question".into(),
                option: Some(0),
            },
            EventKind::QuestionCleared {
                question_id: "q-1".into(),
                why: "milestone completed".into(),
            },
        ] {
            assert!(
                classify(&ev(kind), &base_state(), no_repo()).is_none(),
                "no card for question resolutions"
            );
        }
    }

    #[test]
    fn milestone_blocked_classifies_blocked() {
        let out = classify(
            &ev(EventKind::MilestoneBlocked {
                milestone_id: "ms-1".into(),
                reason: "fix-cycle cap exceeded".into(),
            }),
            &base_state(),
            no_repo(),
        )
        .unwrap();
        assert_eq!(out.class(), NotifyClass::Blocked);
        let Outbound::Blocked(b) = out else { panic!() };
        assert_eq!(b.milestone_id, "ms-1");
        assert_eq!(b.reason, "fix-cycle cap exceeded");
        assert_eq!(b.mission_id, "m-1");
    }

    #[test]
    fn mission_completed_classifies_complete_with_cost() {
        let out = classify(
            &ev(EventKind::MissionCompleted {}),
            &base_state(),
            no_repo(),
        )
        .unwrap();
        let Outbound::Complete(c) = out else { panic!() };
        assert_eq!(c.outcome, Outcome::Completed);
        assert_eq!(c.branch, "kranz/mission-m-1");
        assert_eq!(c.cost_usd, Some(3.5));
        assert!(c.summary.contains("1/1 milestones complete"));
        assert!(c.summary.contains("Rate-limit the notes API"));
    }

    #[test]
    fn mission_failed_carries_reason_and_no_cost_when_zero() {
        let mut state = base_state();
        state.total_cost_usd = 0.0;
        let out = classify(
            &ev(EventKind::MissionFailed {
                reason: "worker exhausted respawns".into(),
            }),
            &state,
            no_repo(),
        )
        .unwrap();
        let Outbound::Complete(c) = out else { panic!() };
        assert_eq!(c.outcome, Outcome::Failed);
        assert_eq!(c.summary, "worker exhausted respawns");
        assert_eq!(c.cost_usd, None);
    }

    #[test]
    fn unremarkable_events_classify_none() {
        assert!(classify(&ev(EventKind::MissionPaused {}), &base_state(), no_repo()).is_none());
        assert!(classify(
            &ev(EventKind::FeatureStarted {
                feature_id: "f-1-1".into()
            }),
            &base_state(),
            no_repo()
        )
        .is_none());
    }
}

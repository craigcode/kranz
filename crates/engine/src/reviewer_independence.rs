//! Approval-pinned review policy, evaluated against resolved dispatch identity.

use crate::config;
use crate::error::{EngineError, Result};
use crate::events::{Event, EventKind};
use crate::types::{
    BackendKind, MissionConfig, MissionState, Plan, ReviewerIndependence, Role, RunResult,
};

#[derive(Debug, PartialEq, Eq)]
pub struct CompletionBlocked {
    pub milestone_id: String,
    pub detail: String,
}

/// Closing work is a policy decision even when the model calls it a skip.
/// Use the latest validation round and its actual successful runs, never a
/// completion status or the pre-dispatch independence decision as evidence.
/// The event order binds the review to the milestone's latest recorded work;
/// new work, changed review context or an integrity failure needs a fresh round.
/// Logs without an approval-pinned policy retain their original behavior.
pub fn check_completion(
    state: &MissionState,
    events: &[Event],
    milestone_id: Option<&str>,
) -> std::result::Result<(), CompletionBlocked> {
    let Some(policy) = state
        .mission
        .reviewer_independence
        .filter(|p| !p.is_empty())
    else {
        return Ok(());
    };
    for milestone in state
        .mission
        .milestones
        .iter()
        .filter(|m| milestone_id.is_none_or(|id| id == m.id))
    {
        let blocked = |detail| CompletionBlocked {
            milestone_id: milestone.id.clone(),
            detail,
        };
        let round = events
            .iter()
            .rposition(|event| {
                matches!(&event.kind,
                    EventKind::MilestoneValidating { milestone_id } if milestone_id == &milestone.id
                )
            })
            .ok_or_else(|| blocked("no recorded validation round for this milestone".into()))?;
        let belongs = |id: &str| milestone.features.iter().any(|feature| feature.id == id);
        let round_events = &events[round + 1..];
        if round_events
            .iter()
            .any(|event| matches!(event.kind, EventKind::PlanRevised { .. }))
        {
            // Revisions preserve completed-prefix work. Reconstruct only on
            // this uncommon path to distinguish a pending-only revision from
            // a change to the goal, contract or scope the reviewer judged.
            let reviewed = crate::reducer::fold(&events[..=round])
                .map_err(|error| blocked(format!("cannot reconstruct reviewed plan: {error}")))?;
            let context = |state: &MissionState| {
                serde_json::to_value((
                    &state.mission.goal,
                    &state.mission.validation_contract,
                    &state.mission.command_grants,
                    &state.mission.touch_set,
                    state
                        .mission
                        .milestones
                        .iter()
                        .find(|m| m.id == milestone.id)
                        .map(|m| {
                            (
                                &m.title,
                                m.features
                                    .iter()
                                    .map(|f| (&f.title, &f.spec, &f.validation_criteria))
                                    .collect::<Vec<_>>(),
                            )
                        }),
                ))
                .map_err(|error| blocked(format!("cannot compare reviewed plan: {error}")))
            };
            if context(&reviewed)? != context(state)? {
                return Err(blocked(
                    "plan revision changed the reviewed milestone context".into(),
                ));
            }
        }
        if round_events.iter().any(|event| match &event.kind {
            EventKind::FeatureStarted { feature_id }
            | EventKind::FeatureProgress { feature_id, .. }
            | EventKind::FeatureCompleted { feature_id, .. }
            | EventKind::FeatureFailed { feature_id, .. } => belongs(feature_id),
            EventKind::WorkerSpawned {
                role: Role::Worker,
                feature_id,
                ..
            } => feature_id.as_deref().is_none_or(belongs),
            EventKind::FixFeatureCreated { milestone_id, .. }
            | EventKind::MilestoneStarted { milestone_id, .. }
            | EventKind::ValidatorTamper { milestone_id, .. } => milestone_id == &milestone.id,
            _ => false,
        }) {
            return Err(blocked(
                "review evidence is stale after work, plan or checkout-integrity changes".into(),
            ));
        }
        for role in [Role::ValidatorScrutiny, Role::ValidatorFunctional] {
            if !policy.requires(role) {
                continue;
            }
            // A failed later attempt must not borrow a previous PASS. The
            // runner records Pass only for a parsed report and clean exit;
            // tamper is checked separately above because it lands afterward.
            let run_id = round_events
                .iter()
                .rev()
                .find_map(|event| match &event.kind {
                    EventKind::WorkerSpawned {
                        run_id,
                        role: run_role,
                        milestone_id: Some(id),
                        ..
                    } if *run_role == role && id == &milestone.id => Some(run_id),
                    _ => None,
                });
            let run = run_id
                .and_then(|id| state.runs.get(id))
                .filter(|run| run.result == Some(RunResult::Pass))
                .ok_or_else(|| {
                    blocked(format!(
                        "{role:?} has no successful review of the latest milestone work"
                    ))
                })?;
            let backend = run.backend.ok_or_else(|| {
                blocked(format!(
                    "{role:?} review has no recorded backend provenance"
                ))
            })?;
            check_dispatch(state, role, backend, &run.model).map_err(blocked)?;
        }
    }
    Ok(())
}

/// A deliberately conservative catalog. A CLI, billing lane, version, effort
/// or model alias alone is not a new family. Unmapped/automatic selections are
/// unknown; ACP cannot bind its attribution string to model selection at all.
pub fn model_family(backend: BackendKind, model: &str) -> Option<&'static str> {
    let model = model.trim().to_ascii_lowercase();
    let claude = ["opus", "sonnet", "haiku", "fable"]
        .iter()
        .any(|name| model == *name || model.starts_with(&format!("claude-{name}-")));
    let gpt = model == "codex" || model.starts_with("gpt-5");
    match backend {
        BackendKind::Claude | BackendKind::Droid | BackendKind::Cursor if claude => {
            Some("anthropic-claude")
        }
        BackendKind::Codex | BackendKind::Cursor if gpt => Some("openai-gpt"),
        BackendKind::Droid if model.starts_with("accounts/fireworks/models/glm-") => {
            Some("zai-glm")
        }
        BackendKind::Kimi
            if matches!(
                model.as_str(),
                "kimi-code/k3"
                    | "kimi-code/kimi-for-coding"
                    | "kimi-code/kimi-for-coding-highspeed"
            ) =>
        {
            Some("moonshot-kimi")
        }
        _ => None,
    }
}

pub fn configured_policy(cfg: &MissionConfig) -> Option<ReviewerIndependence> {
    (!cfg.reviewer_independence.is_empty()).then_some(cfg.reviewer_independence)
}

/// Populate from operator configuration, rejecting planner-authored policy.
pub fn pin_plan(plan: &mut Plan, policy: Option<ReviewerIndependence>) -> Result<()> {
    if plan.reviewer_independence.is_some() && plan.reviewer_independence != policy {
        return Err(EngineError::Config(
            "plan reviewerIndependence differs from the operator policy".into(),
        ));
    }
    plan.reviewer_independence = policy;
    Ok(())
}

pub fn validate_config(cfg: &MissionConfig) -> Result<()> {
    let policy = cfg.reviewer_independence;
    if policy.is_empty() {
        return Ok(());
    }
    for (role, skipped) in [
        (Role::ValidatorScrutiny, cfg.skip_scrutiny),
        (Role::ValidatorFunctional, cfg.skip_functional),
    ] {
        if !policy.requires(role) {
            continue;
        }
        if skipped {
            return Err(EngineError::Config(format!(
                "reviewerIndependence requires {role:?}; its skip flag must be false"
            )));
        }
        let kind = cfg.backend_kind(role);
        let model = config::effective_model(role, kind, &cfg.role(role).model);
        let reviewer = model_family(kind, &model).ok_or_else(|| {
            EngineError::Config(format!(
                "reviewerIndependence: {role:?} model family is unknown"
            ))
        })?;
        let workers = if cfg.worker_candidates.is_empty() {
            vec![(cfg.backend_kind(Role::Worker), cfg.worker.model.as_str())]
        } else {
            cfg.worker_candidates
                .iter()
                .map(|candidate| {
                    config::parse_backend(Some(&candidate.backend))
                        .map(|kind| (kind, candidate.model.as_str()))
                        .map_err(|_| EngineError::Config("unknown worker candidate backend".into()))
                })
                .collect::<Result<Vec<_>>>()?
        };
        for (kind, model) in workers {
            let effective = config::effective_model(Role::Worker, kind, model);
            let worker = model_family(kind, &effective).ok_or_else(|| {
                EngineError::Config("reviewerIndependence: worker model family is unknown".into())
            })?;
            if worker == reviewer {
                return Err(EngineError::Config(format!(
                    "reviewerIndependence: {role:?} and worker share family {worker}"
                )));
            }
        }
    }
    Ok(())
}

/// All attempts count, even failed or discarded ones: their influence on the
/// mission cannot be disproved merely by switching configuration or branches.
pub fn check_dispatch(
    state: &MissionState,
    role: Role,
    backend: BackendKind,
    model: &str,
) -> std::result::Result<Option<String>, String> {
    if !state
        .mission
        .reviewer_independence
        .is_some_and(|p| p.requires(role))
    {
        return Ok(None);
    }
    let reviewer = model_family(backend, model)
        .ok_or_else(|| format!("{role:?} resolved model family is unknown"))?;
    let mut workers = 0;
    // The state uses a BTreeMap: failure selection is stable across replay.
    for run in state.runs.values().filter(|run| run.role == Role::Worker) {
        workers += 1;
        let family = run
            .backend
            .and_then(|kind| model_family(kind, &run.model))
            .ok_or_else(|| format!("worker run {} has unknown model family", run.id))?;
        if reviewer == family {
            return Err(format!(
                "{role:?} resolved to {reviewer}, the same family as worker run {}",
                run.id
            ));
        }
    }
    if workers == 0 {
        return Err("no recorded worker provenance is available".into());
    }
    Ok(Some(format!(
        "{role:?}: {} model {model:?} ({reviewer}) differs from all {} recorded worker attempts",
        backend.as_str(),
        workers
    )))
}

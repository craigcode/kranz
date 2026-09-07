//! Approval-pinned review policy, evaluated against resolved dispatch identity.

use crate::config;
use crate::error::{EngineError, Result};
use crate::types::{BackendKind, MissionConfig, MissionState, Plan, ReviewerIndependence, Role};

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

//! Pure, deterministic fold of the event log into [`MissionState`].
//!
//! `state.json` is only a cache of `fold(events)`; the log is the source of
//! truth. `fold` == `fold(first)` + repeated [`apply`] (property-tested), so
//! the engine can maintain state incrementally while any reader can rebuild
//! it from scratch and get byte-identical JSON.

use crate::error::{EngineError, Result};
use crate::events::{Event, EventKind};
use crate::types::*;
use std::collections::BTreeMap;
use std::path::Path;

/// Reserved `run_id` for `validation.finding` events produced by the engine
/// itself (final contract gate command failures) rather than a validator run.
pub const ENGINE_RUN_ID: &str = "engine";

/// Fold a contiguous event slice into a state. The first event MUST be
/// `mission.created`.
pub fn fold(events: &[Event]) -> Result<MissionState> {
    let first = events
        .first()
        .ok_or_else(|| EngineError::InvalidState("cannot fold an empty event log".to_string()))?;
    let mut state = initial_state(first)?;
    for event in &events[1..] {
        apply(&mut state, event)?;
    }
    Ok(state)
}

/// Apply one event on top of an existing state. `event.seq` must be exactly
/// `state.last_seq + 1` (fold passes contiguous events; anything else is a
/// caller bug or log corruption).
pub fn apply(state: &mut MissionState, event: &Event) -> Result<()> {
    if event.seq != state.last_seq + 1 {
        return Err(EngineError::InvalidState(format!(
            "non-contiguous apply: state at seq {}, event seq {}",
            state.last_seq, event.seq
        )));
    }

    match &event.kind {
        EventKind::MissionCreated { .. } => {
            return Err(EngineError::InvalidState(format!(
                "mission.created at seq {} is only valid as the first event",
                event.seq
            )));
        }

        EventKind::PlanApproved { plan, base_sha } => {
            state.mission.base_sha = base_sha.clone();
            state.mission.goal = plan.goal.clone();
            state.mission.validation_contract = plan.validation_contract.clone();
            state.mission.milestones = plan
                .milestones
                .iter()
                .enumerate()
                .map(|(mi, pm)| Milestone {
                    id: format!("ms-{}", mi + 1),
                    title: pm.title.clone(),
                    features: pm
                        .features
                        .iter()
                        .enumerate()
                        .map(|(fi, pf)| Feature {
                            id: format!("f-{}-{}", mi + 1, fi + 1),
                            title: pf.title.clone(),
                            spec: pf.spec.clone(),
                            validation_criteria: pf.validation_criteria.clone(),
                            origin: FeatureOrigin::Plan,
                            status: FeatureStatus::Pending,
                            worker_runs: Vec::new(),
                            commits: Vec::new(),
                            respawns: 0,
                        })
                        .collect(),
                    status: MilestoneStatus::Pending,
                    fix_cycles: 0,
                    start_sha: None,
                    validator_guidance: None,
                })
                .collect();
            state.mission.command_grants = plan.command_grants.clone();
            state.mission.touch_set = plan.touch_set.clone();
            state.mission.status = MissionStatus::Approved;
            state.latest_plan_revision = 0;
            state.pending_revision = None;
        }

        EventKind::PlanRevisionProposed {
            revision,
            plan,
            instructions,
        } => {
            if *revision == 0 {
                return Err(EngineError::InvalidState(
                    "plan.revision.proposed revision must be >= 1".to_string(),
                ));
            }
            state.latest_plan_revision = state.latest_plan_revision.max(*revision);
            state.pending_revision = Some(PendingRevision {
                revision: *revision,
                plan: plan.clone(),
                instructions: instructions.clone(),
            });
        }

        EventKind::PlanRevised { revision, plan } => {
            if let Some(pending) = &state.pending_revision {
                if pending.revision != *revision {
                    return Err(EngineError::InvalidState(format!(
                        "plan.revised revision {revision} does not match pending revision {}",
                        pending.revision
                    )));
                }
            }
            apply_revised_plan(state, plan, *revision)?;
            state.latest_plan_revision = state.latest_plan_revision.max(*revision);
            state.pending_revision = None;
        }

        EventKind::PlanRevisionRejected { revision, .. } => {
            if let Some(pending) = &state.pending_revision {
                if pending.revision != *revision {
                    return Err(EngineError::InvalidState(format!(
                        "plan.revision.rejected revision {revision} does not match pending revision {}",
                        pending.revision
                    )));
                }
            }
            state.latest_plan_revision = state.latest_plan_revision.max(*revision);
            state.pending_revision = None;
        }

        EventKind::GrantRequested {
            milestone_id,
            kind,
            command,
        } => {
            milestone_mut(state, milestone_id)?; // existence check
            if command.trim().is_empty() {
                return Err(EngineError::InvalidState(
                    "grant.requested command must not be empty".to_string(),
                ));
            }
            state.pending_grant_request = Some(PendingGrantRequest {
                milestone_id: milestone_id.clone(),
                kind: *kind,
                command: command.clone(),
            });
        }

        EventKind::GrantApproved { kind, command } => {
            // Cross-check against the parked request (mirrors PlanRevised): a
            // forged or replayed grant.approved with no matching pending
            // request — or one naming a different kind/target than was
            // requested — must never silently widen an allow-list.
            expect_pending_grant(state, *kind, command, "grant.approved")?;
            // Extend-only, deduped: the approved target joins the list `kind`
            // selects so the retried run clears the boundary.
            let list = match kind {
                GrantKind::Command => &mut state.mission.command_grants,
                GrantKind::TouchPath => &mut state.mission.touch_set,
                GrantKind::WorkerDeny => &mut state.mission.deny_exceptions,
                GrantKind::Egress => &mut state.mission.egress_grants,
            };
            if !list.iter().any(|c| c == command) {
                list.push(command.clone());
            }
            state.pending_grant_request = None;
        }

        EventKind::GrantDenied { kind, command, .. } => {
            expect_pending_grant(state, *kind, command, "grant.denied")?;
            state.pending_grant_request = None;
        }

        EventKind::MilestoneStarted {
            milestone_id,
            start_sha,
        } => {
            if state.executor_tier() == ExecutorTier::Local {
                state.local_executor_milestones += 1;
            }
            let ms = milestone_mut(state, milestone_id)?;
            ms.status = MilestoneStatus::Active;
            ms.start_sha = Some(start_sha.clone());
            if state.mission.status == MissionStatus::Approved {
                state.mission.status = MissionStatus::Running;
            }
        }

        EventKind::FeatureStarted { feature_id } => {
            feature_mut(state, feature_id)?.status = FeatureStatus::Active;
        }

        EventKind::WorkerSpawned {
            run_id,
            role,
            feature_id,
            milestone_id,
            sdk_session_id,
            model,
            quant,
            weight_hash,
            prompt_hash,
            transcript_path,
        } => {
            if state.runs.contains_key(run_id) {
                return Err(EngineError::InvalidState(format!(
                    "duplicate worker.spawned for run '{run_id}'"
                )));
            }
            if let Some(mid) = milestone_id {
                milestone_mut(state, mid)?; // existence check
            }
            if let Some(fid) = feature_id {
                let feature = feature_mut(state, fid)?;
                feature.worker_runs.push(run_id.clone());
                // A 2nd+ run on the same feature is a respawn.
                if feature.worker_runs.len() > 1 {
                    feature.respawns += 1;
                }
            }
            state.runs.insert(
                run_id.clone(),
                WorkerRun {
                    id: run_id.clone(),
                    role: *role,
                    feature_id: feature_id.clone(),
                    milestone_id: milestone_id.clone(),
                    sdk_session_id: sdk_session_id.clone(),
                    model: model.clone(),
                    quant: quant.clone(),
                    weight_hash: weight_hash.clone(),
                    started_at: event.ts,
                    ended_at: None,
                    tokens: TokenUsage::default(),
                    cost_usd: None,
                    transcript_path: transcript_path.clone(),
                    result: None,
                    report: None,
                    prompt_hash: prompt_hash.clone(),
                },
            );
            if state.mission.status == MissionStatus::Approved {
                state.mission.status = MissionStatus::Running;
            }
        }

        EventKind::WorkerMessage { run_id, .. } => {
            run_mut(state, run_id)?; // stream delta: existence check only
        }

        EventKind::WorkerCompleted {
            run_id,
            result,
            tokens,
            cost_usd,
            report,
        } => {
            let run = run_mut(state, run_id)?;
            run.result = Some(*result);
            run.tokens = tokens.clone();
            run.cost_usd = *cost_usd;
            run.report = report.clone();
            run.ended_at = Some(event.ts);
            state.totals.add(tokens);
            state.total_cost_usd += cost_usd.unwrap_or(0.0);
        }

        EventKind::FeatureCompleted {
            feature_id,
            commits,
        } => {
            let feature = feature_mut(state, feature_id)?;
            feature.status = FeatureStatus::Complete;
            feature.commits.extend(commits.iter().cloned());
        }

        EventKind::FeatureFailed { feature_id, .. } => {
            feature_mut(state, feature_id)?.status = FeatureStatus::Failed;
        }

        EventKind::FeatureSkipped { feature_id, .. } => {
            feature_mut(state, feature_id)?.status = FeatureStatus::Skipped;
        }

        EventKind::MilestoneValidating { milestone_id } => {
            milestone_mut(state, milestone_id)?.status = MilestoneStatus::Validating;
        }

        EventKind::ValidationFinding {
            milestone_id,
            run_id,
            ..
        } => {
            // No structural change; validate references as a corruption guard.
            // run_id "engine" is reserved for findings the engine itself
            // produces (final contract gate command failures) — no session
            // exists behind them, so the run lookup is skipped.
            milestone_mut(state, milestone_id)?;
            if run_id != ENGINE_RUN_ID {
                run_mut(state, run_id)?;
            }
        }

        EventKind::ValidatorTamper {
            milestone_id,
            run_id,
            ..
        } => {
            // Audit record of the failed immutability assertion; the
            // accompanying milestone.blocked drives status. Validate
            // references as a corruption guard (mirrors validation.finding).
            milestone_mut(state, milestone_id)?;
            if run_id != ENGINE_RUN_ID {
                run_mut(state, run_id)?;
            }
        }

        EventKind::ValidationSnapshot { milestone_id, .. } => {
            // Audit record of the throwaway checkout a validator session
            // ran in; no structural state change, and no run id exists at
            // emit time (the session starts after the snapshot). Validate
            // the milestone reference as a corruption guard only.
            milestone_mut(state, milestone_id)?;
        }

        EventKind::FixFeatureCreated {
            milestone_id,
            feature,
        } => {
            let ms = milestone_mut(state, milestone_id)?;
            // Reject a colliding feature id loudly rather than silently
            // shadowing (mirrors the worker.spawned duplicate guard). A second
            // re-plan of the same milestone could otherwise mint a duplicate
            // `<ms>-replan-N` id.
            if ms.features.iter().any(|f| f.id == feature.id) {
                return Err(EngineError::InvalidState(format!(
                    "duplicate fixfeature.created for feature '{}'",
                    feature.id
                )));
            }
            // One fix-cycle increment per validation round: the first
            // fixfeature after milestone.validating flips the milestone back
            // to Active; later fixfeatures in the same round arrive while
            // Active and do not increment.
            if ms.status == MilestoneStatus::Validating {
                ms.fix_cycles += 1;
                ms.status = MilestoneStatus::Active;
            }
            ms.features.push(feature.clone());
        }

        EventKind::TierEscalated { milestone_id, .. } => {
            state.config.worker.backend = None;
            state.config.worker.base_url = None;
            state.config.worker.context_budget = None;
            state.config.worker.temperature = None;
            state.escalated_milestones += 1;
            let ms = milestone_mut(state, milestone_id)?;
            ms.status = MilestoneStatus::Active;
            ms.fix_cycles = 0;
        }

        EventKind::MilestoneBlocked { milestone_id, .. } => {
            milestone_mut(state, milestone_id)?.status = MilestoneStatus::Blocked;
            state.mission.status = MissionStatus::Blocked;
        }

        EventKind::MilestoneUnblocked {
            milestone_id,
            validator_guidance,
            ..
        } => {
            let ms = milestone_mut(state, milestone_id)?;
            ms.status = MilestoneStatus::Active;
            // Latest unblock wins (including None — a bare unblock clears
            // guidance left by an earlier one).
            ms.validator_guidance = validator_guidance.clone();
            state.mission.status = MissionStatus::Running;
        }

        EventKind::MilestoneCompleted { milestone_id, .. } => {
            let ms = milestone_mut(state, milestone_id)?;
            ms.status = MilestoneStatus::Complete;
            // Guidance served its purpose; never leak it into a later
            // milestone's (or a re-run's) validators.
            ms.validator_guidance = None;
        }

        EventKind::MissionValidating {} => {
            state.mission.status = MissionStatus::Validating;
        }

        EventKind::MissionPaused {} => {
            state.mission.status = MissionStatus::Paused;
        }

        EventKind::MissionResumed {} => {
            state.mission.status = MissionStatus::Running;
        }

        EventKind::UserMessage { text, .. } => {
            state.pending_user_messages.push(text.clone());
        }

        EventKind::OrchestratorDecision { summary, .. } => {
            state.recent_decisions.push(summary.clone());
            while state.recent_decisions.len() > MAX_RECENT_DECISIONS {
                state.recent_decisions.remove(0);
            }
            // A decision marks the queued user messages as consumed.
            state.pending_user_messages.clear();
        }

        EventKind::SecretRedacted { .. } => {
            // Audit-only: the write boundary already redacted the event that
            // preceded this marker. State shape intentionally does not grow.
        }

        EventKind::ConfigChanged { patch } => {
            let mut value = serde_json::to_value(&state.config)?;
            deep_merge(&mut value, patch);
            state.config = serde_json::from_value(value).map_err(|e| {
                EngineError::Config(format!("config.changed patch produced invalid config: {e}"))
            })?;
        }

        EventKind::MissionCompleted {} => {
            state.mission.status = MissionStatus::Complete;
        }

        EventKind::MissionFailed { .. } => {
            state.mission.status = MissionStatus::Failed;
        }

        EventKind::MissionAbandoned { .. } => {
            state.mission.status = MissionStatus::Abandoned;
        }

        EventKind::WorkspaceProvisioned { provider, .. } => {
            // The last provisioned provider kind is the durable record (D-E);
            // a resume re-provisions and supersedes it with the same value.
            state.workspace_provider = Some(provider.clone());
        }

        EventKind::WorkspaceReadinessReport { .. } => {
            // Audit-only artifact (D-E): the readiness outcome lives on the
            // event trail; state shape intentionally does not grow from it.
        }

        EventKind::WorkspaceTeardown { state: outcome, .. } => {
            // The teardown outcome (ticket workspace-idle-hibernate) folds
            // into the last-known workspace lifecycle — latest transition
            // wins (append-only order), with the event's own ts as the
            // workspace-hours anchor for cost tooling. Teardown events
            // without an outcome (old keep-only logs) leave it untouched.
            if let Some(outcome) = outcome {
                state.workspace_lifecycle = Some(WorkspaceLifecycle {
                    state: outcome.clone(),
                    ts: event.ts,
                });
            }
        }

        EventKind::WorkspaceProviderPinned {
            provider,
            template,
            version,
        } => {
            // The approval-time consent pin (D-B). Emitted once per
            // approve_plan; a retried approval after a failed attempt re-pins
            // (last pin wins).
            state.workspace_pin = Some(WorkspacePin {
                provider: provider.clone(),
                template: template.clone(),
                version: version.clone(),
            });
        }
    }

    state.last_seq = event.seq;
    Ok(())
}

/// Newest-last cap on `MissionState::recent_decisions`.
const MAX_RECENT_DECISIONS: usize = 10;

fn initial_state(event: &Event) -> Result<MissionState> {
    let EventKind::MissionCreated {
        goal,
        base_branch,
        mission_branch,
        config,
    } = &event.kind
    else {
        return Err(EngineError::InvalidState(format!(
            "first event must be mission.created, found '{}'",
            event.kind.type_name()
        )));
    };
    Ok(MissionState {
        mission: Mission {
            id: event.mission_id.clone(),
            goal: goal.clone(),
            validation_contract: Vec::new(),
            milestones: Vec::new(),
            status: MissionStatus::Planning,
            created_at: event.ts,
            base_branch: base_branch.clone(),
            base_sha: None,
            mission_branch: mission_branch.clone(),
            command_grants: Vec::new(),
            touch_set: Vec::new(),
            deny_exceptions: Vec::new(),
            egress_grants: Vec::new(),
        },
        runs: BTreeMap::new(),
        totals: TokenUsage::default(),
        total_cost_usd: 0.0,
        pending_user_messages: Vec::new(),
        recent_decisions: Vec::new(),
        config: config.clone(),
        latest_plan_revision: 0,
        pending_revision: None,
        pending_grant_request: None,
        last_seq: event.seq,
        escalated_milestones: 0,
        local_executor_milestones: 0,
        workspace_provider: None,
        workspace_pin: None,
        workspace_lifecycle: None,
    })
}

/// Assert that `kind`+`command` match the parked `pending_grant_request`. Both
/// `grant.approved` and `grant.denied` gate on this, so a forged or replayed
/// decision event can neither widen an allow-list (approve) nor clear a request
/// the operator never saw (deny) — and can't apply to the WRONG list by
/// swapping the kind. Mirrors the `pending.revision` cross-check that
/// `PlanRevised`/`PlanRevisionRejected` perform.
fn expect_pending_grant(
    state: &MissionState,
    kind: GrantKind,
    command: &str,
    event: &str,
) -> Result<()> {
    match &state.pending_grant_request {
        Some(pending) if pending.kind == kind && pending.command == command => Ok(()),
        Some(pending) => Err(EngineError::InvalidState(format!(
            "{event} {kind:?} {command:?} does not match pending grant {:?} {:?}",
            pending.kind, pending.command
        ))),
        None => Err(EngineError::InvalidState(format!(
            "{event} with no pending grant request"
        ))),
    }
}

fn apply_revised_plan(state: &mut MissionState, plan: &Plan, revision: u32) -> Result<()> {
    ensure_contract_extends(
        &state.mission.validation_contract,
        &plan.validation_contract,
    )?;
    ensure_strings_extend(
        "commandGrants",
        &state.mission.command_grants,
        &plan.command_grants,
    )?;
    ensure_strings_extend("touchSet", &state.mission.touch_set, &plan.touch_set)?;

    let completed_prefix = state
        .mission
        .milestones
        .iter()
        .position(|m| m.status != MilestoneStatus::Complete)
        .unwrap_or(state.mission.milestones.len());
    if plan.milestones.len() < completed_prefix {
        return Err(EngineError::InvalidState(
            "plan.revised drops completed milestones".to_string(),
        ));
    }

    let mut revised_milestones = Vec::new();
    for (idx, existing) in state
        .mission
        .milestones
        .iter()
        .enumerate()
        .take(completed_prefix)
    {
        let Some(plan_milestone) = plan.milestones.get(idx) else {
            return Err(EngineError::InvalidState(format!(
                "plan.revised drops completed milestone '{}'",
                existing.title
            )));
        };
        if !completed_milestone_matches(existing, plan_milestone) {
            return Err(EngineError::InvalidState(format!(
                "plan.revised alters completed milestone '{}'",
                existing.title
            )));
        }
        revised_milestones.push(existing.clone());
    }

    for (idx, plan_milestone) in plan.milestones.iter().enumerate().skip(completed_prefix) {
        if let Some(existing) = state.mission.milestones.get(idx) {
            revised_milestones.push(merge_revised_milestone(existing, plan_milestone, revision));
        } else {
            revised_milestones.push(new_plan_milestone(idx, plan_milestone));
        }
    }

    state.mission.goal = plan.goal.clone();
    state.mission.validation_contract = plan.validation_contract.clone();
    state.mission.command_grants = plan.command_grants.clone();
    state.mission.touch_set = plan.touch_set.clone();
    state.mission.milestones = revised_milestones;
    Ok(())
}

/// Validate that a `PlanRevised { revision, plan }` event would fold cleanly
/// onto `state`, WITHOUT mutating it. The orchestrator calls this before it
/// durably appends the event — `emit` appends before it folds — so a revision
/// the reducer would reject is refused up front instead of poisoning the
/// append-only log. A failed fold on replay would otherwise error on every
/// subsequent load and permanently brick the mission.
pub fn dry_run_revised_plan(state: &MissionState, plan: &Plan, revision: u32) -> Result<()> {
    apply_revised_plan(&mut state.clone(), plan, revision)
}

fn ensure_contract_extends(existing: &[Assertion], revised: &[Assertion]) -> Result<()> {
    for old in existing {
        let Some(new) = revised.iter().find(|a| a.id == old.id) else {
            return Err(EngineError::InvalidState(format!(
                "plan.revised removes validation assertion '{}'",
                old.id
            )));
        };
        if old.statement != new.statement || old.check != new.check || old.command != new.command {
            return Err(EngineError::InvalidState(format!(
                "plan.revised weakens or changes validation assertion '{}'",
                old.id
            )));
        }
    }
    Ok(())
}

fn ensure_strings_extend(label: &str, existing: &[String], revised: &[String]) -> Result<()> {
    for old in existing {
        if !revised.iter().any(|new| new == old) {
            return Err(EngineError::InvalidState(format!(
                "plan.revised removes {label} entry '{old}'"
            )));
        }
    }
    Ok(())
}

fn completed_milestone_matches(existing: &Milestone, revised: &PlanMilestone) -> bool {
    existing.title.trim() == revised.title.trim()
        && completed_features_match(&existing.features, &revised.features)
}

/// Whether a completed milestone's features are reproduced UNCHANGED in a
/// revised plan: same count, and same title/spec/validation-criteria in order,
/// compared TRIMMED. This is the single source of truth for the "completed
/// work is frozen" rule — the orchestrator's pre-emit gate
/// (`completed_features_unchanged`) delegates here so the gate and the reducer
/// can never diverge. The leniency is deliberate: a regenerated plan will not
/// echo incidental whitespace back byte-for-byte, and whitespace is not a
/// content change. An exact compare here would let the gate accept a revision
/// the reducer then rejects, and because `emit` appends before it folds, that
/// leaves an unfoldable event in the append-only log and bricks the mission.
pub(crate) fn completed_features_match(existing: &[Feature], revised: &[PlanFeature]) -> bool {
    existing.len() == revised.len()
        && existing.iter().zip(revised).all(|(a, b)| {
            a.title.trim() == b.title.trim()
                && a.spec.trim() == b.spec.trim()
                && a.validation_criteria.len() == b.validation_criteria.len()
                && a.validation_criteria
                    .iter()
                    .zip(&b.validation_criteria)
                    .all(|(x, y)| x.trim() == y.trim())
        })
}

fn merge_revised_milestone(
    existing: &Milestone,
    revised: &PlanMilestone,
    revision: u32,
) -> Milestone {
    let mut features = Vec::new();
    let mut new_count = 0usize;
    for feature in &existing.features {
        let matching = revised
            .features
            .iter()
            .find(|candidate| norm_title(&candidate.title) == norm_title(&feature.title));
        match (feature.status, matching) {
            (FeatureStatus::Pending, Some(plan_feature)) => {
                let mut updated = feature.clone();
                updated.title = plan_feature.title.clone();
                updated.spec = plan_feature.spec.clone();
                updated.validation_criteria = plan_feature.validation_criteria.clone();
                features.push(updated);
            }
            (FeatureStatus::Pending, None) => {
                let mut skipped = feature.clone();
                skipped.status = FeatureStatus::Skipped;
                features.push(skipped);
            }
            _ => features.push(feature.clone()),
        }
    }

    for plan_feature in &revised.features {
        let already_present = existing
            .features
            .iter()
            .any(|feature| norm_title(&feature.title) == norm_title(&plan_feature.title));
        if !already_present {
            new_count += 1;
            features.push(Feature {
                id: format!("{}-rev-{revision}-{new_count}", existing.id),
                title: plan_feature.title.clone(),
                spec: plan_feature.spec.clone(),
                validation_criteria: plan_feature.validation_criteria.clone(),
                origin: FeatureOrigin::Plan,
                status: FeatureStatus::Pending,
                worker_runs: Vec::new(),
                commits: Vec::new(),
                respawns: 0,
            });
        }
    }

    Milestone {
        id: existing.id.clone(),
        title: revised.title.clone(),
        features,
        status: existing.status,
        fix_cycles: existing.fix_cycles,
        start_sha: existing.start_sha.clone(),
        // A revision rebuilds the milestone but does not unblock it — folded
        // operator guidance survives, exactly like fix_cycles and start_sha.
        validator_guidance: existing.validator_guidance.clone(),
    }
}

fn new_plan_milestone(idx: usize, plan_milestone: &PlanMilestone) -> Milestone {
    Milestone {
        id: format!("ms-{}", idx + 1),
        title: plan_milestone.title.clone(),
        features: plan_milestone
            .features
            .iter()
            .enumerate()
            .map(|(fi, feature)| Feature {
                id: format!("f-{}-{}", idx + 1, fi + 1),
                title: feature.title.clone(),
                spec: feature.spec.clone(),
                validation_criteria: feature.validation_criteria.clone(),
                origin: FeatureOrigin::Plan,
                status: FeatureStatus::Pending,
                worker_runs: Vec::new(),
                commits: Vec::new(),
                respawns: 0,
            })
            .collect(),
        status: MilestoneStatus::Pending,
        fix_cycles: 0,
        start_sha: None,
        validator_guidance: None,
    }
}

fn norm_title(title: &str) -> String {
    title.trim().to_ascii_lowercase()
}

fn milestone_mut<'a>(state: &'a mut MissionState, id: &str) -> Result<&'a mut Milestone> {
    state
        .mission
        .milestones
        .iter_mut()
        .find(|m| m.id == id)
        .ok_or_else(|| {
            EngineError::InvalidState(format!("event references unknown milestone '{id}'"))
        })
}

fn feature_mut<'a>(state: &'a mut MissionState, id: &str) -> Result<&'a mut Feature> {
    state
        .mission
        .milestones
        .iter_mut()
        .flat_map(|m| m.features.iter_mut())
        .find(|f| f.id == id)
        .ok_or_else(|| {
            EngineError::InvalidState(format!("event references unknown feature '{id}'"))
        })
}

fn run_mut<'a>(state: &'a mut MissionState, id: &str) -> Result<&'a mut WorkerRun> {
    state
        .runs
        .get_mut(id)
        .ok_or_else(|| EngineError::InvalidState(format!("event references unknown run '{id}'")))
}

/// Recursive JSON merge: objects merge key-by-key, anything else in the patch
/// replaces the base value wholesale.
fn deep_merge(base: &mut serde_json::Value, patch: &serde_json::Value) {
    use serde_json::Value;
    match (base, patch) {
        (Value::Object(base_map), Value::Object(patch_map)) => {
            for (key, patch_value) in patch_map {
                deep_merge(
                    base_map.entry(key.clone()).or_insert(Value::Null),
                    patch_value,
                );
            }
        }
        (base_slot, patch_value) => *base_slot = patch_value.clone(),
    }
}

// ---------------------------------------------------------------------------
// Snapshot cache (state.json)
// ---------------------------------------------------------------------------

/// Serialize the state pretty-printed to a sibling tmp file, then atomically
/// rename over `path` so readers never observe a half-written snapshot.
pub fn write_snapshot(state: &MissionState, path: &Path) -> Result<()> {
    let file_name = path.file_name().ok_or_else(|| {
        EngineError::InvalidState(format!("snapshot path {} has no file name", path.display()))
    })?;
    let tmp = path.with_file_name(format!("{}.tmp", file_name.to_string_lossy()));

    let json = serde_json::to_string_pretty(state)?;
    {
        let mut file = std::fs::File::create(&tmp)?;
        std::io::Write::write_all(&mut file, json.as_bytes())?;
        file.sync_data()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Read a snapshot previously written by [`write_snapshot`]. A symlinked
/// `state.json` — or any symlinked component above it — is refused (P1
/// mission-path-no-follow), never read through: mission-layout paths are
/// pinned capability-relative from the trusted repo-root anchor (7th-pass
/// review); out-of-layout paths (test scratch) use the weaker
/// canonicalize tier — see [`crate::paths::open_read_nofollow`].
pub fn read_snapshot(path: &Path) -> Result<MissionState> {
    use std::io::Read;
    let mut content = String::new();
    crate::paths::open_read_nofollow(path)?.read_to_string(&mut content)?;
    Ok(serde_json::from_str(&content)?)
}

#[cfg(test)]
mod executor_tier_tests {
    use super::*;
    use crate::events::EventKind;

    fn created_event(config: MissionConfig) -> Event {
        Event {
            seq: 1,
            ts: chrono::Utc::now(),
            mission_id: "m-test".to_string(),
            kind: EventKind::MissionCreated {
                goal: "ship the thing".to_string(),
                base_branch: "main".to_string(),
                mission_branch: "kranz/mission-m-test".to_string(),
                config,
            },
        }
    }

    #[test]
    fn executor_routing_applies_local_worker_backend_folds_to_local_tier() {
        let mut config = MissionConfig::default();
        config.worker.backend = Some("local".to_string());

        let state = fold(&[created_event(config)]).unwrap();

        assert_eq!(state.executor_tier(), ExecutorTier::Local);
    }

    #[test]
    fn executor_routing_applies_non_local_worker_backend_folds_to_frontier_tier() {
        let mut config = MissionConfig::default();
        config.worker.backend = Some("codex".to_string());

        let state = fold(&[created_event(config)]).unwrap();

        assert_eq!(state.executor_tier(), ExecutorTier::Frontier);
    }

    #[test]
    fn validator_stays_frontier_regardless_of_worker_routing() {
        let mut config = MissionConfig::default();
        config.worker.backend = Some("local".to_string());

        let state = fold(&[created_event(config)]).unwrap();

        assert_eq!(
            state.config.backend_kind(Role::ValidatorScrutiny),
            BackendKind::Claude
        );
        assert_eq!(
            state.config.backend_kind(Role::ValidatorFunctional),
            BackendKind::Claude
        );
    }
}

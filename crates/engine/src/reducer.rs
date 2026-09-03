//! Pure, deterministic fold of the event log into [`MissionState`].
//!
//! `state.json` is only a cache of `fold(events)`; the log is the source of
//! truth. `fold` == `fold(first)` + repeated [`apply`] (property-tested), so
//! the engine can maintain state incrementally while any reader can rebuild
//! it from scratch and get byte-identical JSON.

use crate::error::{EngineError, Result};
use crate::events::{Event, EventKind};
use crate::types::*;
use std::collections::{BTreeMap, BTreeSet};
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
            // Status guard, the sibling of `expect_pending_grant` below
            // (audit 2026-09-01 H6). `approve_plan` only ever emits this from
            // `Planning`, so a `plan.approved` folded on top of a running,
            // blocked, or completed mission did not come from the approval
            // path: it is a late forgery appended to the log, and applying it
            // would replace the milestone set, the contract, the touch set,
            // and the command grants wholesale. Ignored rather than a fold
            // error, so one bad line cannot make an existing mission
            // permanently unreadable.
            if state.mission.status != MissionStatus::Planning {
                tracing::warn!(
                    seq = event.seq,
                    status = ?state.mission.status,
                    "ignoring plan.approved outside Planning status"
                );
                state.last_seq = event.seq;
                return Ok(());
            }
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
            // The Flight Rules approval pin (KRZ-342 D-E) folds with the plan
            // it was approved with — the mission's standards authority from
            // here on.
            state.mission.standards_manifest = plan.standards_manifest.as_deref().cloned();
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
            candidate,
            executor_route: _,
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
                // A 2nd+ run on the same feature is a respawn — EXCEPT a
                // dispatch-pool candidate (KRZ-303): the N sibling streams
                // are ONE logical dispatch of the unit, not N-1 retries, and
                // the pool path has no judgement-driven respawn loop at all,
                // so counting them would silently deplete `max_respawns`.
                if feature.worker_runs.len() > 1 && candidate.is_none() {
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
                    candidate: candidate.clone(),
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

        EventKind::WorkerEgressDenied { run_id, .. } => {
            // Audit-only runtime evidence. The durable event deliberately
            // adds no mutable state shape; consumers select the relevant run
            // ids directly from the validated log. Still validate the run
            // reference so a hand-edited event cannot cite a nonexistent
            // session.
            run_mut(state, run_id)?;
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

        EventKind::FeatureFailed {
            feature_id,
            commits,
            ..
        } => {
            let feature = feature_mut(state, feature_id)?;
            feature.status = FeatureStatus::Failed;
            // Record any commits the failure landed on the mission branch:
            // the fix-feature supersession guard reads `commits.is_empty()`
            // to tell a failed-COMMITLESS feature (re-proposable — the
            // m-eee81f auth-death wedge) from failed-with-real-work (started;
            // a duplicate fixfeature.created must reject).
            feature.commits.extend(commits.iter().cloned());
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

        EventKind::ValidationConfirm {
            milestone_id,
            local_run_id,
            confirm_run_id,
            ..
        } => {
            // Audit-only record (KRZ-206b, the gate.result additive
            // template): the local-vs-frontier comparison drives no state
            // transition — a disagreement's finding already flows through
            // validation.finding, and the miss rate reads this event back
            // off the log, so state shape does not grow. Validate all
            // references as a corruption guard (mirrors validation.finding):
            // both run ids name real sessions (the local primary and the
            // frontier confirmation), so a hand-edited confirm cannot cite
            // a run the log never recorded.
            milestone_mut(state, milestone_id)?;
            run_mut(state, local_run_id)?;
            run_mut(state, confirm_run_id)?;
        }

        EventKind::ValidationPtyTranscript { milestone_id, .. } => {
            // Audit-only record (ticket pty-functional-validation): the
            // verdict reaches the round through the functional validator's
            // evidence block, not through this event, so it drives no state
            // transition (mirrors validation.snapshot). No run id exists at
            // emit time — the evidence pass is engine-run — so only the
            // milestone reference is validated as a corruption guard.
            milestone_mut(state, milestone_id)?;
        }

        EventKind::GateResult { .. } => {
            // Audit-only record (KRZ-312): one gate evaluation — id, ladder
            // position, verdict, artefact handle. Gate results drive no
            // state transition (the advisory posture of contract gates is
            // unchanged: verdicts inform, they never block), and the ladder
            // a replay reconstructs is read from the events themselves, so
            // state shape intentionally does not grow. And unlike
            // validation.finding there is no milestone/run reference on the
            // payload to validate as a corruption guard — the arm is a pure
            // no-op, exactly like secret.redacted below.
        }

        EventKind::HookGateFired { run_id, .. } => {
            // Record-only (KRZ-302, the gate.result additive template): the
            // in-process hook verdict already happened inside the session,
            // and the engine-side sweep remains the authoritative layer —
            // so this drives no state transition and state shape does not
            // grow. Validate the run reference as a corruption guard
            // (mirrors validation.finding); the run id is engine-stamped at
            // fold time, so this cannot be aimed at a run the log never
            // recorded.
            run_mut(state, run_id)?;
        }

        EventKind::DivergenceNoted {
            unit, candidates, ..
        } => {
            // Audit record of the candidate comparison (KRZ-304); the
            // accompanying milestone.blocked drives the park, and the
            // `diverged` verdict is deliberately NEVER folded into any
            // state a decision could key on — agreement between models is a
            // signal to log, never a criterion to trust. Validate
            // references as a corruption guard (mirrors validation.finding):
            // the unit names a feature, every candidate a recorded run.
            feature_mut(state, unit)?;
            for candidate in candidates {
                run_mut(state, &candidate.run_id)?;
            }
        }

        EventKind::DivergenceResolved { unit, .. } => {
            // The judgement record (KRZ-304). The unit joins the folded
            // resolution set — the engine's restart-safe memory for "this
            // unit was already judged" (at most one resolution per unit;
            // the set insert keeps a duplicated hand-written event benign).
            // Reference validation as a corruption guard, as above.
            feature_mut(state, unit)?;
            state.resolved_divergence_units.insert(unit.clone());
        }

        EventKind::FixFeatureCreated {
            milestone_id,
            feature,
        } => {
            let ms = milestone_mut(state, milestone_id)?;
            if let Some(existing) = ms.features.iter().find(|f| f.id == feature.id) {
                // A duplicate with an IDENTICAL proposal is an idempotent
                // replay — a retried emission after a crash between emit
                // and fold (mission m-83d1ed). Event-sourced recovery must
                // no-op it, not brick — and it must NOT skip the last_seq
                // advance at the tail, or the next event fails contiguity
                // (the m-83d1ed wedge's second form).
                if existing.title == feature.title
                    && existing.spec == feature.spec
                    && existing.validation_criteria == feature.validation_criteria
                {
                    // fall through to the tail: seq advances, state unchanged
                } else {
                    // A duplicate with a DIFFERENT payload is an implicit
                    // SUPERSESSION when the prior feature never produced work:
                    // an unstarted (Pending) or failed-and-commitless feature
                    // can be re-proposed by a re-plan — this is the normal
                    // shape after new findings (m-83d1ed re-proposed the same
                    // id twice). Failed-with-runs-but-no-commits is the same
                    // class: runs that never committed produced no work (the
                    // m-eee81f wedge — three infra-failed runs made
                    // `worker_runs` non-empty and bricked every re-proposal);
                    // their records stay in the log. A feature whose run is
                    // in flight is Active, so runs alone are not the "started"
                    // signal. The successor REPLACES the prior payload in
                    // place and restarts as Pending; the original payload is
                    // not lost — it lives in this same event log (the first
                    // fixfeature.created). Once a feature has started,
                    // committed, or completed, a revision is genuinely
                    // shadowing and stays loudly invalid.
                    let idx = ms
                        .features
                        .iter()
                        .position(|f| f.id == feature.id)
                        .expect("found above");
                    let existing = &ms.features[idx];
                    let prior_started = matches!(
                        existing.status,
                        FeatureStatus::Active | FeatureStatus::Complete | FeatureStatus::Skipped
                    ) || !existing.commits.is_empty();
                    if prior_started {
                        return Err(EngineError::InvalidState(format!(
                        "duplicate fixfeature.created for feature '{}' with a different payload",
                        feature.id
                    )));
                    }
                    tracing::info!(
                        feature_id = %feature.id,
                        "fixfeature re-proposed before any work: folding as implicit supersession"
                    );
                    if ms.status == MilestoneStatus::Validating {
                        ms.fix_cycles += 1;
                        ms.status = MilestoneStatus::Active;
                    }
                    let mut successor = feature.clone();
                    successor.status = FeatureStatus::Pending;
                    ms.features[idx] = successor;
                }
            } else {
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

        EventKind::WorkerEscalated { run_id, .. } => {
            // Record-only (KRZ-331, the gate.result additive template): the
            // worker's escalation request is provenance — the judgement turn
            // (the frontier advisor) acts on the report, and nothing a
            // decision could key on changes here: the validator route, the
            // executor tier, the respawn budget, and every milestone status
            // are deliberately untouched, so a worker escalation can never
            // bypass the floor's validator requirements (contrast
            // tier.escalated above, the orchestrator-initiated tier flip,
            // which DOES rewrite worker config). Validate the run reference
            // as a corruption guard (mirrors hook.gate.fired): the run id is
            // engine-stamped at emit time, so this cannot be aimed at a run
            // the log never recorded.
            run_mut(state, run_id)?;
        }

        EventKind::QuestionOpened {
            question_id,
            role,
            text,
            options,
            run_id,
            feature_id,
            milestone_id,
        } => {
            // The pending-decision projection's open edge (ticket
            // structured-human-question-events). NOT record-only: the open
            // question IS state a surface renders and an answer cross-checks
            // against, so it folds onto `pending_questions` — but it gates
            // NOTHING in the run loop (contrast grant.requested's park).
            // Validate references as a corruption guard (mirrors
            // validation.finding), then dedupe like fixfeature.created: a
            // duplicated open with an IDENTICAL payload is an idempotent
            // replay (no double-push, no id-counter bump); the same id with
            // a DIFFERENT payload is shadowing and stays loudly invalid.
            if question_id.trim().is_empty() {
                return Err(EngineError::InvalidState(
                    "question.opened question id must not be empty".to_string(),
                ));
            }
            if text.trim().is_empty() {
                return Err(EngineError::InvalidState(format!(
                    "question.opened {question_id} text must not be empty"
                )));
            }
            if let Some(run_id) = run_id {
                run_mut(state, run_id)?;
            }
            if let Some(feature_id) = feature_id {
                feature_mut(state, feature_id)?;
            }
            if let Some(milestone_id) = milestone_id {
                milestone_mut(state, milestone_id)?;
            }
            if let Some(existing) = state
                .pending_questions
                .iter()
                .find(|q| q.question_id == *question_id)
            {
                let identical = existing.role == *role
                    && existing.text == *text
                    && existing.options == *options
                    && existing.run_id == *run_id
                    && existing.feature_id == *feature_id
                    && existing.milestone_id == *milestone_id;
                if !identical {
                    return Err(EngineError::InvalidState(format!(
                        "duplicate question.opened for '{question_id}' with a different payload"
                    )));
                }
                // fall through to the tail: seq advances, state unchanged
            } else {
                state.question_count += 1;
                state.pending_questions.push(PendingQuestion {
                    question_id: question_id.clone(),
                    role: *role,
                    text: text.clone(),
                    options: options.clone(),
                    run_id: run_id.clone(),
                    feature_id: feature_id.clone(),
                    milestone_id: milestone_id.clone(),
                });
            }
        }

        EventKind::QuestionAnswered {
            question_id,
            answer,
            ..
        } => {
            // The projection's answer edge: cross-check against the parked
            // question (mirrors expect_pending_grant — a stale or forged
            // answer for a question that is not open fails the fold), remove
            // it, then route the answer onto `pending_user_messages` — the
            // EXISTING consult path (D-X: answers ride the msg machinery,
            // never a new delivery mechanism), so the orchestrator's next
            // user-message consult consumes the answer and a restart replays
            // it from the log alone.
            let question = take_pending_question(state, question_id, "question.answered")?;
            state.pending_user_messages.push(format!(
                "answer to question {} (\"{}\"): {}",
                question.question_id, question.text, answer
            ));
        }

        EventKind::QuestionCleared { question_id, .. } => {
            // The projection's clear edge: same parked-question cross-check
            // as the answer (a clear for a question that is not open is
            // corruption, never a silent no-op).
            take_pending_question(state, question_id, "question.cleared")?;
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

        EventKind::StandardsResolved { .. }
        | EventKind::StandardsDrifted { .. }
        | EventKind::StandardsWaiverApproved { .. }
        | EventKind::StandardsAttestationApproved { .. } => {
            // Audit-only (KRZ-342 D-H; KRZ-344 D-I): the pin itself folds
            // with plan.approved; these events are the queryable provenance,
            // refusal, and waiver evidence. The coverage fold joins waivers
            // straight from the log — state shape intentionally does not
            // grow.
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
            standards_manifest: None,
            // The seed-time route record (ticket routing-rules-config): the
            // folded task class exists only on THIS event's goal, so the
            // decision is derived here, once — deterministically equal to
            // what create applied (routing::seed_executor_route).
            executor_route: crate::routing::seed_executor_route(config, goal),
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
        pending_questions: Vec::new(),
        question_count: 0,
        last_seq: event.seq,
        escalated_milestones: 0,
        local_executor_milestones: 0,
        workspace_provider: None,
        workspace_pin: None,
        workspace_lifecycle: None,
        resolved_divergence_units: BTreeSet::new(),
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

/// Remove and return the parked question `question_id` names, or fail the
/// fold. Both `question.answered` and `question.cleared` gate on this
/// (mirrors [`expect_pending_grant`]): a stale, replayed, or forged
/// resolution for a question that is not open — never asked, already
/// answered, already cleared — is corruption, not a silent no-op.
fn take_pending_question(
    state: &mut MissionState,
    question_id: &str,
    event: &str,
) -> Result<PendingQuestion> {
    let Some(index) = state
        .pending_questions
        .iter()
        .position(|q| q.question_id == question_id)
    else {
        return Err(EngineError::InvalidState(format!(
            "{event} for question '{question_id}' that is not open"
        )));
    };
    Ok(state.pending_questions.remove(index))
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
    // The Flight Rules pin (KRZ-342 D-E) is NEVER re-read from a revision:
    // the planner never authors policy, and no revision flow re-validates a
    // carried manifest against the trusted source — folding one would let a
    // re-plan substitute weakened policy into the consent artifact. The
    // approval-time pin stands for the mission's life; an envelope escape is
    // caught by the final-validation check (which re-resolves the pinned
    // base snapshot against actual paths) and by the merge drift check.
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
/// replaces the base value wholesale. `pub(crate)` so the provenance replay
/// (provenance.rs) evolves its tracked config by the SAME merge — a second
/// spelling of "how a config.changed patch applies" could drift from this one.
pub(crate) fn deep_merge(base: &mut serde_json::Value, patch: &serde_json::Value) {
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
    let tmp_name = format!("{}.tmp", file_name.to_string_lossy());
    let (parent, pinned_name) = crate::paths::open_parent_nofollow(path)?;

    // Refuse hostile leaves before opening. The no-follow open below is the
    // authoritative race-safe check; this metadata pass gives a useful error
    // for directories, fifos, devices, and pre-existing symlinks.
    for (name, display) in [
        (pinned_name.as_os_str(), path.to_path_buf()),
        (
            std::ffi::OsStr::new(&tmp_name),
            path.with_file_name(&tmp_name),
        ),
    ] {
        match parent.symlink_metadata(name) {
            Ok(metadata) if metadata.file_type().is_file() => {}
            Ok(_) => {
                return Err(EngineError::InvalidState(format!(
                    "refusing snapshot write through non-regular path {}",
                    display.display()
                )))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }

    let json = serde_json::to_string_pretty(state)?;
    {
        use cap_fs_ext::OpenOptionsFollowExt as _;
        use cap_primitives::fs::FollowSymlinks;
        let mut options = cap_std::fs::OpenOptions::new();
        options
            .write(true)
            .create(true)
            .truncate(true)
            .follow(FollowSymlinks::No);
        let mut file = parent.open_with(&tmp_name, &options)?.into_std();
        std::io::Write::write_all(&mut file, json.as_bytes())?;
        file.sync_data()?;
    }
    parent.rename(&tmp_name, &parent, &pinned_name)?;
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

#[cfg(test)]
mod hook_gate_projection_tests {
    use super::*;
    use crate::events::EventKind;

    fn event(seq: u64, kind: EventKind) -> Event {
        Event {
            seq,
            ts: chrono::Utc::now(),
            mission_id: "m-test".to_string(),
            kind,
        }
    }

    fn spawned(seq: u64, run_id: &str) -> Event {
        event(
            seq,
            EventKind::WorkerSpawned {
                run_id: run_id.to_string(),
                role: Role::Worker,
                feature_id: None,
                milestone_id: None,
                candidate: None,
                executor_route: None,
                sdk_session_id: "s-1".to_string(),
                model: "m".to_string(),
                quant: "n/a".to_string(),
                weight_hash: None,
                prompt_hash: "h".to_string(),
                transcript_path: "runs/r-1.jsonl".to_string(),
            },
        )
    }

    /// The hook.gate.fired fold arm is record-only (KRZ-302): the run
    /// reference is validated as a corruption guard, and state shape does
    /// not grow — the mission's course is unchanged by the in-process
    /// verdict (the engine-side sweep remains the authoritative layer).
    #[test]
    fn hook_gate_projection_event_folds_record_only() {
        let created = event(
            1,
            EventKind::MissionCreated {
                goal: "g".to_string(),
                base_branch: "main".to_string(),
                mission_branch: "kranz/mission-m-test".to_string(),
                config: MissionConfig::default(),
            },
        );
        let fired = event(
            3,
            EventKind::HookGateFired {
                run_id: "r-1".to_string(),
                gate: "out-of-contract-write".to_string(),
                hook_event: "PreToolUse".to_string(),
                tool: "Write".to_string(),
                subject: "docs/oops.md".to_string(),
                verdict: "blocked".to_string(),
                detail: Some("outside the touch set".to_string()),
            },
        );

        let with_hook = fold(&[created.clone(), spawned(2, "r-1"), fired]).unwrap();
        let without_hook = fold(&[created, spawned(2, "r-1")]).unwrap();

        // No state transition: identical mission status, run set, and run
        // outcome with and without the hook event.
        assert_eq!(with_hook.mission.status, without_hook.mission.status);
        assert_eq!(with_hook.runs.len(), 1);
        assert!(with_hook.runs["r-1"].result.is_none());
        assert_eq!(
            with_hook.mission.milestones.len(),
            without_hook.mission.milestones.len()
        );
    }

    /// The run reference is a corruption guard: a hook.gate.fired naming a
    /// run the log never recorded refuses the fold (the run id is
    /// engine-stamped at fold time, so this can only be log corruption).
    #[test]
    fn hook_gate_projection_event_with_unknown_run_is_refused() {
        let created = event(
            1,
            EventKind::MissionCreated {
                goal: "g".to_string(),
                base_branch: "main".to_string(),
                mission_branch: "kranz/mission-m-test".to_string(),
                config: MissionConfig::default(),
            },
        );
        let bogus = event(
            2,
            EventKind::HookGateFired {
                run_id: "no-such-run".to_string(),
                gate: "out-of-contract-write".to_string(),
                hook_event: "PreToolUse".to_string(),
                tool: "Write".to_string(),
                subject: "docs/oops.md".to_string(),
                verdict: "blocked".to_string(),
                detail: None,
            },
        );
        assert!(fold(&[created, bogus]).is_err());
    }
}

#[cfg(test)]
mod routing_abstraction_tests {
    use super::*;
    use crate::events::EventKind;

    fn event(seq: u64, kind: EventKind) -> Event {
        Event {
            seq,
            ts: chrono::Utc::now(),
            mission_id: "m-test".to_string(),
            kind,
        }
    }

    fn created_with_local_worker() -> Event {
        let mut config = MissionConfig::default();
        config.worker.backend = Some("local".to_string());
        event(
            1,
            EventKind::MissionCreated {
                goal: "g".to_string(),
                base_branch: "main".to_string(),
                mission_branch: "kranz/mission-m-test".to_string(),
                config,
            },
        )
    }

    fn spawned(seq: u64, run_id: &str) -> Event {
        event(
            seq,
            EventKind::WorkerSpawned {
                run_id: run_id.to_string(),
                role: Role::Worker,
                feature_id: None,
                milestone_id: None,
                candidate: None,
                executor_route: None,
                sdk_session_id: "s-1".to_string(),
                model: "m".to_string(),
                quant: "n/a".to_string(),
                weight_hash: None,
                prompt_hash: "h".to_string(),
                transcript_path: "runs/r-1.jsonl".to_string(),
            },
        )
    }

    /// The worker.escalated fold arm is record-only (KRZ-331, the gate.result
    /// template): a worker escalation NEVER bypasses the floor's validator
    /// requirements — the validator route, the executor tier, and every
    /// decision-keyed counter fold exactly as if the event were absent
    /// (contrast tier.escalated, the orchestrator-initiated valve, which
    /// deliberately rewrites worker config). Only the run reference is
    /// validated, as a corruption guard (mirrors hook.gate.fired).
    #[test]
    fn routing_abstraction_escalation_folds_record_only_leaving_validators_untouched() {
        let escalated = event(
            3,
            EventKind::WorkerEscalated {
                run_id: "r-1".to_string(),
                feature_id: "f-1-1".to_string(),
                from: ExecutorTier::Local,
                to: ExecutorTier::Frontier,
                reason: "spec ambiguity beyond my confidence".to_string(),
            },
        );

        let with = fold(&[created_with_local_worker(), spawned(2, "r-1"), escalated]).unwrap();
        let without = fold(&[created_with_local_worker(), spawned(2, "r-1")]).unwrap();

        // The WHOLE config is identical with and without the escalation —
        // validator backends are inside it, so this pins "validator route
        // unaffected" exactly, not by a sampled field.
        assert_eq!(with.config, without.config);
        assert_eq!(
            with.config.backend_kind(Role::ValidatorScrutiny),
            BackendKind::Claude
        );
        assert_eq!(
            with.config.backend_kind(Role::ValidatorFunctional),
            BackendKind::Claude
        );
        // The worker escalation never flips the executor tier (that flip is
        // tier.escalated's job, and it is orchestrator-initiated only).
        assert_eq!(with.executor_tier(), ExecutorTier::Local);
        // No state transition of any kind: same mission status, same run
        // set, same escalation counters.
        assert_eq!(with.mission.status, without.mission.status);
        assert_eq!(with.runs.len(), without.runs.len());
        assert_eq!(with.escalated_milestones, without.escalated_milestones);
        assert_eq!(
            with.local_executor_milestones,
            without.local_executor_milestones
        );
    }

    /// The run reference is a corruption guard: a worker.escalated naming a
    /// run the log never recorded refuses the fold (the run id is
    /// engine-stamped at emit time, so this can only be log corruption).
    #[test]
    fn routing_abstraction_escalation_with_unknown_run_is_refused() {
        let bogus = event(
            2,
            EventKind::WorkerEscalated {
                run_id: "no-such-run".to_string(),
                feature_id: "f-1-1".to_string(),
                from: ExecutorTier::Local,
                to: ExecutorTier::Frontier,
                reason: "r".to_string(),
            },
        );
        let mut config = MissionConfig::default();
        config.worker.backend = Some("local".to_string());
        let created = event(
            1,
            EventKind::MissionCreated {
                goal: "g".to_string(),
                base_branch: "main".to_string(),
                mission_branch: "kranz/mission-m-test".to_string(),
                config,
            },
        );
        assert!(fold(&[created, bogus]).is_err());
    }
}

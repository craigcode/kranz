use super::reasons::*;
use crate::events::{Event, EventKind};
use crate::types::*;
use chrono::{Duration, Utc};

fn at() -> chrono::DateTime<Utc> {
    "2026-09-22T03:00:00Z".parse().unwrap()
}
fn events(kinds: Vec<EventKind>) -> Vec<Event> {
    kinds
        .into_iter()
        .enumerate()
        .map(|(i, kind)| Event {
            seq: i as u64 + 1,
            ts: at() + Duration::seconds(i as i64),
            mission_id: "m-reasons".into(),
            kind,
        })
        .collect()
}
fn created() -> EventKind {
    EventKind::MissionCreated {
        goal: "fixture".into(),
        base_branch: "main".into(),
        mission_branch: "kranz/mission-m-reasons".into(),
        config: MissionConfig::default(),
    }
}
fn approved() -> EventKind {
    EventKind::PlanApproved {
        plan: serde_json::from_value(serde_json::json!({"goal":"fixture", "validationContract":[], "milestones":[{"title":"unit", "features":[]}]})).unwrap(), base_sha: None,
    }
}
fn block(context: Option<BlockContext>) -> EventKind {
    EventKind::MilestoneBlocked {
        milestone_id: "ms-1".into(),
        reason: "missing credential / assertion failed — deliberately ambiguous prose".into(),
        block_context: context,
    }
}
fn validator() -> EventKind {
    EventKind::WorkerSpawned {
        run_id: "v-1".into(),
        role: Role::ValidatorFunctional,
        feature_id: None,
        milestone_id: Some("ms-1".into()),
        candidate: None,
        executor_route: None,
        sdk_session_id: "session".into(),
        model: "fixture".into(),
        backend: None,
        quant: "n/a".into(),
        weight_hash: None,
        prompt_hash: "hash".into(),
        transcript_path: "runs/validator.jsonl".into(),
    }
}
fn finding(run_id: &str) -> EventKind {
    EventKind::ValidationFinding {
        milestone_id: "ms-1".into(),
        run_id: run_id.into(),
        finding: Finding {
            subject: "authorization".into(),
            severity: "major".into(),
            evidence: "Wrong credential was accepted by the check.".into(),
            suggested_fix: String::new(),
            class: String::new(),
            rule: None,
        },
    }
}
fn reason_fold(events: &[Event]) -> MissionReasons {
    crate::reducer::fold(events).unwrap();
    super::mission_outcomes("m-reasons", events).outcome_reasons
}

#[test]
fn outcome_reasons_preserve_typed_causes_repair_history_and_current_state() {
    let log = events(vec![
        created(),
        approved(),
        block(Some(BlockContext::engine(BlockCause::Authentication))),
        EventKind::MilestoneUnblocked {
            milestone_id: "ms-1".into(),
            reason: "credential supplied".into(),
            block_context: Some(BlockContext::engine(BlockCause::Authentication)),
            validator_guidance: None,
        },
        validator(),
        finding("v-1"),
        finding(crate::reducer::ENGINE_RUN_ID),
        EventKind::MissionPaused {},
        EventKind::MissionResumed {},
        EventKind::MissionCompleted {},
    ]);
    let bytes = serde_json::to_vec(&log).unwrap();
    let state_before = serde_json::to_vec(&crate::reducer::fold(&log).unwrap()).unwrap();
    let reasons = reason_fold(&log);
    assert_eq!(reasons, reason_fold(&log));
    assert_eq!(reasons.current_status, Some(MissionStatus::Complete));
    assert_eq!(
        reasons.observations[0].category,
        Category::EnvironmentPrerequisite
    );
    assert_eq!(reasons.observations[0].state, ResolutionState::Resolved);
    assert_eq!(reasons.observations[0].resolution_seq, Some(4));
    assert_eq!(reasons.observations[1].category, Category::ReportedDefect);
    assert_eq!(reasons.observations[1].run_id.as_deref(), Some("v-1"));
    assert_eq!(
        reasons.observations[2].category,
        Category::Unknown,
        "an engine-run failing command cannot establish a defect's cause"
    );
    assert_eq!(reasons.observations[3].category, Category::Interrupted);
    let report = report(vec![reasons], vec![], Some((1, at() + Duration::days(1)))).unwrap();
    assert_eq!(report.task_classes[0].missions, 1);
    assert_eq!(report.task_classes[0].mixed_missions, 1);
    assert!(report.task_classes[0]
        .counts
        .iter()
        .all(|c| c.missions == 1 && c.share == Some(1.0)));
    assert_eq!(bytes, serde_json::to_vec(&log).unwrap());
    assert_eq!(
        state_before,
        serde_json::to_vec(&crate::reducer::fold(&log).unwrap()).unwrap()
    );
}

#[test]
fn outcome_reasons_legacy_and_future_causes_stay_unknown_without_prose_matching() {
    let unknown: BlockContext =
        serde_json::from_value(serde_json::json!({"owner":"engine", "cause":"future-cause"}))
            .unwrap();
    for context in [None, Some(unknown), Some(BlockContext::WORKSPACE_GATE)] {
        let reasons = reason_fold(&events(vec![created(), approved(), block(context)]));
        assert_eq!(reasons.observations[0].category, Category::Unknown);
        assert_eq!(reasons.current_status, Some(MissionStatus::Blocked));
        assert_eq!(reasons.observations[0].seq, 3);
    }
    let mut corrupt = events(vec![
        created(),
        approved(),
        block(Some(BlockContext::engine(BlockCause::Authentication))),
    ]);
    corrupt[2].seq = 99;
    let reasons = super::mission_outcomes("m-reasons", &corrupt).outcome_reasons;
    assert_eq!(reasons.current_status, None);
    assert!(reasons
        .observations
        .iter()
        .all(|r| r.category == Category::Unknown));
}

#[test]
fn outcome_reasons_pending_and_cancelled_are_not_success_or_implicit_consent() {
    let mut log = events(vec![
        created(),
        approved(),
        EventKind::GrantRequested {
            milestone_id: "ms-1".into(),
            kind: GrantKind::Command,
            command: "check".into(),
        },
    ]);
    let pending = reason_fold(&log);
    assert_eq!(
        pending.observations[0].category,
        Category::HumanPolicyBoundary
    );
    assert_eq!(pending.observations[0].state, ResolutionState::Unresolved);
    assert_eq!(
        pending.observations[0].actor, None,
        "legacy grants name no actor"
    );
    log.push(Event {
        seq: 4,
        ts: at() + Duration::seconds(3),
        mission_id: "m-reasons".into(),
        kind: EventKind::MissionAbandoned {
            reason: "operator stopped".into(),
        },
    });
    let cancelled = reason_fold(&log);
    assert_eq!(cancelled.current_status, Some(MissionStatus::Abandoned));
    assert_eq!(cancelled.observations[0].state, ResolutionState::Closed);
    assert_eq!(cancelled.observations[1].category, Category::Cancelled);
    assert!(crate::reducer::fold(&log)
        .unwrap()
        .mission
        .command_grants
        .is_empty());
}

#[test]
fn outcome_reasons_window_has_unique_mission_denominators_and_inclusive_edges() {
    let mut one = reason_fold(&events(vec![
        created(),
        approved(),
        block(Some(BlockContext::engine(BlockCause::Authentication))),
    ]));
    one.task_class = "bug-fix".into();
    one.latest_event_at = Some(at());
    let mut two = one.clone();
    two.mission_id = "m-two".into();
    two.latest_event_at = Some(at() - Duration::days(1));
    two.observations.push(two.observations[0].clone());
    let mut old = one.clone();
    old.mission_id = "m-old".into();
    old.latest_event_at = Some(at() - Duration::days(1) - Duration::seconds(1));
    let mut future = one.clone();
    future.mission_id = "m-future".into();
    future.latest_event_at = Some(at() + Duration::seconds(1));
    let result = report(
        vec![one, two, old, future],
        vec!["m-unavailable".into()],
        Some((1, at())),
    )
    .unwrap();
    assert_eq!(result.missions.len(), 2);
    assert_eq!(result.task_classes[0].missions, 2);
    assert_eq!(result.task_classes[0].counts[0].missions, 2);
    assert_eq!(result.task_classes[0].counts[0].observations, 3);
    assert_eq!(result.unavailable_logs, ["m-unavailable"]);
    assert!(report(vec![], vec![], Some((u64::MAX, at()))).is_err());
}

#[test]
fn outcome_reasons_api_payload_is_additive_and_export_reuses_the_same_fold() {
    let tmp = tempfile::tempdir().unwrap();
    let log = events(vec![
        created(),
        approved(),
        block(Some(BlockContext::engine(BlockCause::Authentication))),
        EventKind::MissionAbandoned {
            reason: "stopped".into(),
        },
    ]);
    let paths = crate::paths::MissionPaths::new(tmp.path(), "m-reasons");
    std::fs::create_dir_all(paths.mission_dir()).unwrap();
    let lines: String = log
        .iter()
        .map(|e| format!("{}\n", serde_json::to_string(e).unwrap()))
        .collect();
    std::fs::write(paths.events_file(), &lines).unwrap();
    let options = super::OutcomesOptions::default();
    let result = super::compute_outcomes_with_options(tmp.path(), &options).unwrap();
    assert_eq!(
        result.outcome_reasons.as_ref().unwrap().missions,
        [reason_fold(&log)]
    );
    let text = render_text(result.outcome_reasons.as_ref().unwrap());
    assert!(text.contains("environment prerequisite"));
    assert!(text.contains("event #3"));
    for id in ["m-empty", "m-missing", "m-corrupt"] {
        let missing = crate::paths::MissionPaths::new(tmp.path(), id);
        std::fs::create_dir_all(missing.mission_dir()).unwrap();
        if id != "m-missing" {
            std::fs::write(
                missing.events_file(),
                if id == "m-empty" {
                    ""
                } else {
                    "invalid JSON\n"
                },
            )
            .unwrap();
        }
    }
    let with_missing = super::compute_outcomes_with_options(tmp.path(), &options).unwrap();
    let report = with_missing.outcome_reasons.unwrap();
    assert_eq!(report.task_classes[0].missions, 1);
    assert_eq!(
        report.unavailable_logs,
        ["m-corrupt", "m-empty", "m-missing"]
    );
    let mut legacy = serde_json::to_value(&result).unwrap();
    legacy.as_object_mut().unwrap().remove("outcomeReasons");
    let legacy: super::Outcomes = serde_json::from_value(legacy).unwrap();
    assert!(legacy.outcome_reasons.is_none());
    let bundle = crate::evidence_bundle::assemble_evidence_bundle(tmp.path(), "m-reasons").unwrap();
    let bytes = &bundle
        .files
        .iter()
        .find(|f| f.path == "cost.json")
        .unwrap()
        .bytes;
    let cost: crate::evidence_bundle::MissionCostSummary = serde_json::from_slice(bytes).unwrap();
    assert_eq!(cost.outcome_reasons, Some(reason_fold(&log)));
    assert_eq!(std::fs::read_to_string(paths.events_file()).unwrap(), lines);
    assert!(
        !paths.control_dir().exists(),
        "reporting must not start work or answer permission requests"
    );
}

#[test]
fn outcome_reasons_external_results_keep_attempt_policy_and_actor() {
    use crate::gate_evaluation::{lifecycle::*, protocol::*};
    use crate::live_permission::Actor;
    use crate::pack::evaluator::{Enforcement, Kind};
    for mode in ["defect", "error", "mechanical", "pending"] {
        let path = format!(
            "{}/schemas/fixtures/gate-v1/milestone-validation/request.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let mut request = Request::from_bytes(&std::fs::read(path).unwrap()).unwrap();
        request.params.deadline =
            (at() + Duration::minutes(5)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        request.params.mission_id = Id::try_from("m-reasons".to_string()).unwrap();
        let policy = Policy {
            kind: if mode == "defect" {
                Kind::Judgment
            } else {
                Kind::Mechanical
            },
            enforcement: Enforcement::Blocking,
            mission_policy_digest: Digest::of(b"policy"),
            mechanical_prerequisites_passed: true,
        };
        request.params.binding.policy_digest = policy.digest();
        let params = request.params.clone();
        let requested = Requested {
            request,
            policy,
            retained_inputs: vec![],
            permission_request_id: None,
        };
        let result = EvaluationResult {
            schema_version: 1,
            evaluation_id: params.evaluation_id.clone(),
            attempt_id: params.attempt_id.clone(),
            binding: params.binding.clone(),
            evidence_digest: params.evidence.digest.clone(),
            status: if mode == "pending" {
                Status::Escalate
            } else {
                Status::Judged
            },
            verdict: if mode == "pending" {
                None
            } else {
                Some(Verdict::Fail)
            },
            rationale: "Assertion rejected the candidate: expected 403, received 200.".into(),
            artifacts: vec![],
            confidence: None,
            findings: Some(vec![crate::gate_evaluation::protocol::Finding {
                id: Id::try_from("f-1".to_string()).unwrap(),
                severity: Severity::High,
                summary: "Authorization assertion failed".into(),
                evidence: vec![Anchor {
                    artifact_id: Id::try_from("source".to_string()).unwrap(),
                    digest: Digest::of(b"probe"),
                    line_start: Some(1),
                    line_end: Some(1),
                }],
            }]),
        };
        let finished = Finished {
            attempt_id: params.attempt_id.clone(),
            outcome: if mode == "error" {
                Outcome::Error {
                    message: "credential / assertion failure: ambiguous diagnostic".into(),
                }
            } else {
                Outcome::Evaluated {
                    raw_stdout_digest: Digest::of(b"result"),
                    result: Box::new(result),
                }
            },
            exit_code: if mode == "error" { None } else { Some(0) },
            cleanup_confirmed: true,
            artifacts: vec![],
        };
        let mut record = Record::new(requested.clone(), at() + Duration::seconds(2), 3).unwrap();
        record
            .finish(finished.clone(), at() + Duration::seconds(3))
            .unwrap();
        let consent = (mode != "pending").then_some(Consent {
            actor: Actor::LocalMutationCapability,
            allow: true,
            reference: "explicit-stage-decision".into(),
        });
        let resolution = Resolution {
            id: Id::try_from("resolution-1".to_string()).unwrap(),
            attempt_id: params.attempt_id.clone(),
            binding: params.binding,
            disposition: record.disposition(consent.as_ref()).unwrap(),
            rationale: "Policy applied to the recorded check".into(),
            consent,
        };
        assert_eq!(
            resolution.disposition,
            if mode == "pending" {
                Disposition::RequireHuman
            } else {
                Disposition::Block
            }
        );
        let log = events(vec![
            created(),
            approved(),
            EventKind::MilestoneStarted {
                milestone_id: "ms-1".into(),
                start_sha: "a".repeat(40),
            },
            EventKind::GateEvaluationRequested {
                evaluation: Box::new(requested),
            },
            EventKind::GateEvaluationFinished {
                evaluation: Box::new(finished),
            },
            EventKind::GateResolutionRecorded { resolution },
        ]);
        let reasons = reason_fold(&log);
        assert_eq!(reasons.observations.len(), 2);
        assert_eq!(
            reasons.observations[0].category,
            match mode {
                "defect" => Category::ReportedDefect,
                "pending" => Category::HumanPolicyBoundary,
                _ => Category::Unknown,
            }
        );
        let boundary = &reasons.observations[1];
        assert_eq!(boundary.category, Category::HumanPolicyBoundary);
        assert_eq!(
            boundary.attempt_id.as_deref(),
            Some(params.attempt_id.as_str())
        );
        assert_eq!(boundary.stage, Some(Stage::MilestoneValidation));
        assert_eq!(
            boundary.actor,
            (mode != "pending").then_some(Actor::LocalMutationCapability)
        );
        assert_eq!(
            boundary.state,
            if mode == "pending" {
                ResolutionState::Unresolved
            } else {
                ResolutionState::Recorded
            }
        );
        assert!(crate::reducer::fold(&log)
            .unwrap()
            .gate_evaluations
            .values()
            .all(|r| r.consumed.is_none()));
    }
}

#[test]
fn outcome_reasons_permission_expiry_and_denial_preserve_actual_authority() {
    use crate::live_permission::*;
    let action = serde_json::json!({"command":"check"});
    let options = vec![serde_json::json!({"optionId":"no", "kind":"reject_once"})];
    let request = Request::new(
        Proposal {
            id: "p-1".into(),
            engine_session_id: "session".into(),
            peer_session_id: "peer".into(),
            peer_request_id: serde_json::json!(1),
            tool_call_id: "tool-1".into(),
            action_digest: digest(&action).unwrap(),
            options_digest: digest(&options).unwrap(),
            action,
            options,
            observed_at: at(),
            deadline: at() + Duration::seconds(60),
            prohibition: Some("Nonwaivable policy boundary".into()),
        },
        Binding {
            mission_id: "m-reasons".into(),
            run_id: "v-1".into(),
            workspace: std::env::temp_dir().to_string_lossy().into_owned(),
            plan_digest: "a".repeat(64),
            policy_digest: "b".repeat(64),
        },
    )
    .unwrap();
    let mut worker = validator();
    if let EventKind::WorkerSpawned { role, .. } = &mut worker {
        *role = Role::Worker;
    }
    let mut log = events(vec![
        created(),
        approved(),
        worker,
        EventKind::PermissionRequested {
            request: request.clone(),
        },
    ]);
    let pending = reason_fold(&log);
    assert_eq!(pending.observations[0].state, ResolutionState::Unresolved);
    let expired = report(
        vec![pending],
        vec![],
        Some((1, at() + Duration::seconds(60))),
    )
    .unwrap();
    assert_eq!(
        expired.missions[0].observations[0].state,
        ResolutionState::Expired
    );
    assert!(crate::reducer::fold(&log).unwrap().permissions["p-1"]
        .resolution
        .is_none());
    log.push(Event {
        seq: 5,
        ts: at() + Duration::seconds(4),
        mission_id: "m-reasons".into(),
        kind: EventKind::PermissionResolved {
            resolution: Resolution {
                request_id: "p-1".into(),
                binding_digest: request.binding_digest,
                allow: false,
                actor: Actor::Policy,
                reason: "Policy denied the proposed tool".into(),
            },
        },
    });
    let denied = reason_fold(&log);
    assert_eq!(denied.observations[0].resolution_seq, Some(5));
    let row = &denied.observations[1];
    assert_eq!(row.category, Category::HumanPolicyBoundary);
    assert_eq!(row.actor, Some(Actor::Policy));
    assert_eq!(row.permission_request_id.as_deref(), Some("p-1"));
    assert_eq!(row.run_id.as_deref(), Some("v-1"));
    assert!(
        !crate::reducer::fold(&log).unwrap().permissions["p-1"]
            .resolution
            .as_ref()
            .unwrap()
            .allow
    );
}

#[test]
fn outcome_reasons_ignore_other_missions_without_losing_current_state() {
    let mut log = events(vec![
        created(),
        approved(),
        block(Some(BlockContext::engine(BlockCause::Authentication))),
    ]);
    let expected = reason_fold(&log);
    let mut foreign = log[0].clone();
    foreign.mission_id = "unrelated".into();
    log.insert(1, foreign);
    assert_eq!(
        super::mission_outcomes("m-reasons", &log).outcome_reasons,
        expected
    );
}

#[test]
fn outcome_reasons_replaced_requests_do_not_remain_pending() {
    let log = events(vec![
        created(),
        approved(),
        EventKind::GrantRequested {
            milestone_id: "ms-1".into(),
            kind: GrantKind::Command,
            command: "first".into(),
        },
        EventKind::GrantRequested {
            milestone_id: "ms-1".into(),
            kind: GrantKind::Command,
            command: "second".into(),
        },
        EventKind::GrantDenied {
            kind: GrantKind::Command,
            command: "second".into(),
            reason: "No".into(),
        },
        block(Some(BlockContext::engine(BlockCause::Authentication))),
        block(Some(BlockContext::engine(BlockCause::Authentication))),
        EventKind::MilestoneUnblocked {
            milestone_id: "ms-1".into(),
            reason: "fixed".into(),
            block_context: None,
            validator_guidance: None,
        },
    ]);
    let reasons = reason_fold(&log);
    assert_eq!(reasons.observations[0].state, ResolutionState::Superseded);
    assert_eq!(reasons.observations[0].resolution_seq, Some(4));
    assert_eq!(reasons.observations[3].state, ResolutionState::Superseded);
    assert!(reasons
        .observations
        .iter()
        .all(|r| r.state != ResolutionState::Unresolved));
    assert_eq!(
        report(vec![reasons], vec![], None).unwrap().task_classes[0].unresolved_missions,
        0
    );
}

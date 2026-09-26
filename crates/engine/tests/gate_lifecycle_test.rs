use chrono::{DateTime, Duration, SecondsFormat, Utc};
use kranz_engine::events::{Event, EventKind};
use kranz_engine::gate_evaluation::{lifecycle::*, protocol::*};
use kranz_engine::live_permission::Actor;
use kranz_engine::pack::evaluator::{Enforcement, Kind};
use kranz_engine::reducer;

fn at() -> DateTime<Utc> {
    "2026-09-16T12:00:00Z".parse().unwrap()
}
fn requested(stage: &str) -> Requested {
    let path = format!(
        "{}/schemas/fixtures/gate-v1/{stage}/request.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let mut request = Request::from_bytes(&std::fs::read(path).unwrap()).unwrap();
    request.params.deadline =
        (at() + Duration::minutes(5)).to_rfc3339_opts(SecondsFormat::Secs, true);
    request.params.mission_id = Id::try_from("m-lifecycle".to_string()).unwrap();
    let policy = Policy {
        kind: Kind::Mechanical,
        enforcement: Enforcement::Blocking,
        mission_policy_digest: Digest::of(b"mission policy"),
        mechanical_prerequisites_passed: true,
    };
    request.params.binding.policy_digest = policy.digest();
    Requested {
        request,
        policy,
        retained_inputs: vec![],
        permission_request_id: None,
    }
}
fn finished(request: &Requested, status: Status, verdict: Option<Verdict>) -> Finished {
    let params = &request.request.params;
    Finished {
        attempt_id: params.attempt_id.clone(),
        outcome: Outcome::Evaluated {
            raw_stdout_digest: Digest::of(b"raw fixture"),
            result: Box::new(EvaluationResult {
                schema_version: 1,
                evaluation_id: params.evaluation_id.clone(),
                attempt_id: params.attempt_id.clone(),
                binding: params.binding.clone(),
                evidence_digest: params.evidence.digest.clone(),
                status,
                verdict,
                rationale: "fixture result".into(),
                artifacts: vec![],
                findings: None,
                confidence: None,
            }),
        },
        exit_code: Some(0),
        cleanup_confirmed: true,
        artifacts: vec![],
    }
}
fn consent() -> Consent {
    Consent {
        actor: Actor::LocalMutationCapability,
        allow: true,
        reference: "explicit-stage-decision".into(),
    }
}
fn resolution(record: &Record, consent: Option<Consent>) -> Resolution {
    Resolution {
        id: Id::try_from("resolution-1".to_string()).unwrap(),
        attempt_id: record.requested.request.params.attempt_id.clone(),
        binding: record.requested.request.params.binding.clone(),
        disposition: record.disposition(consent.as_ref()).unwrap(),
        rationale: "engine policy applied".into(),
        consent,
    }
}

#[test]
fn gate_lifecycle_all_stages_keep_results_separate_from_required_consent() {
    for (stage, action, needs_human) in [
        ("plan-approval", Action::ApprovePlan, true),
        ("command-permission", Action::AnswerPermission, true),
        ("milestone-validation", Action::AcceptMilestone, false),
        ("final-gate", Action::AcceptDeliverable, false),
        ("merge", Action::AdvanceLocalBase, true),
    ] {
        let request = requested(stage);
        let finish = finished(&request, Status::Judged, Some(Verdict::Pass));
        let mut record = Record::new(request, at(), 2).unwrap();
        record.finish(finish.clone(), at()).unwrap();
        assert!(record.finish(finish, at()).is_err());
        assert_eq!(
            record.disposition(None).unwrap(),
            if needs_human {
                Disposition::RequireHuman
            } else {
                Disposition::Proceed
            }
        );
        let resolved = resolution(&record, needs_human.then(consent));
        record.resolve(resolved.clone(), at()).unwrap();
        assert!(record.resolve(resolved.clone(), at()).is_err());
        let consumed = Consumed {
            attempt_id: resolved.attempt_id,
            resolution_id: resolved.id,
            rechecked_binding: resolved.binding,
            action,
        };
        let mut changed = consumed.clone();
        changed.rechecked_binding.subject_digest = Digest::of(b"later edit");
        assert!(record.consume(changed, at()).is_err());
        let mut wrong_stage = consumed.clone();
        wrong_stage.action = if stage == "merge" {
            Action::AcceptDeliverable
        } else {
            Action::AdvanceLocalBase
        };
        assert!(record.consume(wrong_stage, at()).is_err());
        assert!(record
            .consume(consumed.clone(), at() + Duration::minutes(5))
            .is_err());
        record.consume(consumed.clone(), at()).unwrap();
        assert!(record.consume(consumed, at()).is_err());
    }
}

#[test]
fn gate_lifecycle_error_escalation_floors_and_advisory_never_fabricate_a_pass() {
    for enforcement in [Enforcement::Advisory, Enforcement::Blocking] {
        for mode in ["fail", "error", "escalate", "unclean"] {
            let mut request = requested("final-gate");
            request.policy.enforcement = enforcement;
            request.request.params.binding.policy_digest = request.policy.digest();
            let mut finish = finished(&request, Status::Judged, Some(Verdict::Fail));
            if mode == "escalate" {
                finish = finished(&request, Status::Escalate, None);
            }
            if mode == "error" || mode == "unclean" {
                finish.outcome = Outcome::Error {
                    message: "could not evaluate".into(),
                };
                finish.exit_code = None;
            }
            if mode == "unclean" {
                finish.cleanup_confirmed = false;
            }
            let mut record = Record::new(request, at(), 2).unwrap();
            record.finish(finish, at()).unwrap();
            let expected = if mode == "unclean" {
                Disposition::Block
            } else if mode == "escalate" {
                Disposition::RequireHuman
            } else if enforcement == Enforcement::Blocking {
                Disposition::Block
            } else {
                Disposition::Proceed
            };
            assert_eq!(
                record.disposition(Some(&consent())).unwrap(),
                expected,
                "{enforcement:?} {mode}"
            );
            let mut forged = resolution(&record, None);
            forged.disposition = if expected == Disposition::Proceed {
                Disposition::Block
            } else {
                Disposition::Proceed
            };
            assert!(record.resolve(forged, at()).is_err());
        }
    }
    let mut request = requested("final-gate");
    request.policy.kind = Kind::Judgment;
    request.policy.mechanical_prerequisites_passed = false;
    request.request.params.binding.policy_digest = request.policy.digest();
    assert!(Record::new(request, at(), 2).is_err());
}

#[test]
fn gate_lifecycle_replay_keeps_old_logs_unchanged_and_never_consumes_a_pending_result() {
    let old: Vec<Event> = include_str!("fixtures/gate-results-ladder.events.jsonl")
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let old_state = reducer::fold(&old).unwrap();
    let value = serde_json::to_value(&old_state).unwrap();
    assert!(value.get("gateEvaluations").is_none());
    assert!(value.get("consumedGateResolutions").is_none());
    let request = requested("plan-approval");
    let finish = finished(&request, Status::Judged, Some(Verdict::Pass));
    let events = vec![
        Event {
            seq: 1,
            ts: at(),
            mission_id: "m-lifecycle".into(),
            kind: EventKind::MissionCreated {
                goal: "fixture".into(),
                base_branch: "main".into(),
                mission_branch: "kranz/fixture".into(),
                config: Default::default(),
            },
        },
        Event {
            seq: 2,
            ts: at(),
            mission_id: "m-lifecycle".into(),
            kind: EventKind::GateEvaluationRequested {
                evaluation: Box::new(request.clone()),
            },
        },
        Event {
            seq: 3,
            ts: at(),
            mission_id: "m-lifecycle".into(),
            kind: EventKind::GateEvaluationFinished {
                evaluation: Box::new(finish),
            },
        },
    ];
    let once = reducer::fold(&events).unwrap();
    let twice = reducer::fold(&events).unwrap();
    assert_eq!(
        serde_json::to_value(&once).unwrap(),
        serde_json::to_value(&twice).unwrap()
    );
    let record = &once.gate_evaluations[request.request.params.attempt_id.as_str()];
    assert!(record.resolution.is_none());
    assert!(record.consumed.is_none());
    assert!(once.consumed_gate_resolutions.is_empty());
    let mut incremental = reducer::fold(&events[..1]).unwrap();
    for event in &events[1..] {
        reducer::apply(&mut incremental, event).unwrap();
    }
    assert_eq!(
        serde_json::to_value(once).unwrap(),
        serde_json::to_value(incremental).unwrap()
    );
}

#[test]
fn gate_lifecycle_rejects_wrong_policy_deadline_process_and_correlation() {
    let mut request = requested("final-gate");
    request.policy.enforcement = Enforcement::Advisory;
    assert!(Record::new(request, at(), 2).is_err());
    let request = requested("final-gate");
    assert!(Record::new(request.clone(), at() + Duration::minutes(5), 2).is_err());
    for change in [
        "attempt", "binding", "evidence", "exit", "cleanup", "expiry",
    ] {
        let mut record = Record::new(request.clone(), at(), 2).unwrap();
        let mut finish = finished(&request, Status::Judged, Some(Verdict::Pass));
        let Outcome::Evaluated { result, .. } = &mut finish.outcome else {
            unreachable!()
        };
        match change {
            "attempt" => result.attempt_id = Id::try_from("foreign".to_string()).unwrap(),
            "binding" => result.binding.registration_digest = Digest::of(b"different checker"),
            "evidence" => result.evidence_digest = Digest::of(b"different evidence"),
            "exit" => finish.exit_code = Some(1),
            "cleanup" => finish.cleanup_confirmed = false,
            _ => {}
        }
        let time = if change == "expiry" {
            at() + Duration::minutes(5)
        } else {
            at()
        };
        assert!(record.finish(finish, time).is_err(), "{change}");
        assert!(record.finished.is_none());
    }
}

#[test]
fn gate_lifecycle_bundle_names_missing_and_changed_retained_bytes() {
    use kranz_engine::evidence_bundle::assemble_evidence_bundle;
    use kranz_engine::paths::MissionPaths;
    use kranz_engine::provenance::ArtefactStatus;
    let dir = tempfile::tempdir().unwrap();
    let paths = MissionPaths::new(dir.path(), "m-lifecycle");
    let folder = paths.runs_dir().join("gates/attempt-1");
    std::fs::create_dir_all(&folder).unwrap();
    let retained = b"checked output [REDACTED]\n\xff";
    std::fs::write(folder.join("present.txt"), retained).unwrap();
    let mut request = requested("plan-approval");
    request.retained_inputs = ["present.txt", "missing.txt"]
        .iter()
        .map(|name| RetainedArtifact {
            path: WirePath::try_from(format!("runs/gates/attempt-1/{name}")).unwrap(),
            raw_digest: Digest::of(b"different original bytes, not retained"),
            retained_digest: Digest::of(retained),
            retained_bytes: retained.len() as u64,
            transformation: "fixture-redaction-v1".into(),
        })
        .collect();
    let events = [
        Event {
            seq: 1,
            ts: at(),
            mission_id: "m-lifecycle".into(),
            kind: EventKind::MissionCreated {
                goal: "fixture".into(),
                base_branch: "main".into(),
                mission_branch: "kranz/fixture".into(),
                config: Default::default(),
            },
        },
        Event {
            seq: 2,
            ts: at(),
            mission_id: "m-lifecycle".into(),
            kind: EventKind::GateEvaluationRequested {
                evaluation: Box::new(request),
            },
        },
    ];
    let mut log = String::new();
    for event in events {
        log.push_str(&serde_json::to_string(&event).unwrap());
        log.push('\n');
    }
    std::fs::write(paths.events_file(), log).unwrap();
    let bundle = assemble_evidence_bundle(dir.path(), "m-lifecycle").unwrap();
    assert_eq!(
        bundle,
        assemble_evidence_bundle(dir.path(), "m-lifecycle").unwrap()
    );
    for entry in bundle
        .manifest
        .entries
        .iter()
        .filter(|entry| entry.source.contains("runs/gates/"))
    {
        if entry.source.ends_with("missing.txt") {
            assert_eq!(entry.status, Some(ArtefactStatus::Unresolved));
            assert!(entry.path.is_none());
            assert!(entry.sha256.is_none());
        } else {
            assert_eq!(entry.status, Some(ArtefactStatus::Resolved));
            let bytes = &bundle
                .files
                .iter()
                .find(|file| Some(&file.path) == entry.path.as_ref())
                .unwrap()
                .bytes;
            assert_eq!(bytes, String::from_utf8_lossy(retained).as_bytes());
            assert_eq!(
                format!("sha256:{}", entry.sha256.as_ref().unwrap()),
                Digest::of(bytes).as_str()
            );
        }
    }
    assert_eq!(
        bundle
            .manifest
            .entries
            .iter()
            .filter(|entry| entry.source.contains("runs/gates/"))
            .count(),
        2
    );
    let summary = &bundle
        .files
        .iter()
        .find(|file| file.path == "summary.md")
        .unwrap()
        .bytes;
    assert!(String::from_utf8_lossy(summary).contains("interrupted or pending evaluation"));
    std::fs::write(folder.join("present.txt"), "substituted bytes").unwrap();
    let changed = assemble_evidence_bundle(dir.path(), "m-lifecycle").unwrap();
    let entry = changed
        .manifest
        .entries
        .iter()
        .find(|entry| entry.source.ends_with("present.txt"))
        .unwrap();
    assert_eq!(entry.status, Some(ArtefactStatus::Unresolved));
    assert!(entry.sha256.is_none());
}

#[test]
fn gate_lifecycle_invocation_joins_exact_live_authority_and_cannot_relabel_consent() {
    use kranz_engine::live_permission as permission;
    use serde_json::json;
    let first = Event {
        seq: 1,
        ts: at(),
        mission_id: "m-lifecycle".into(),
        kind: EventKind::MissionCreated {
            goal: "fixture".into(),
            base_branch: "main".into(),
            mission_branch: "kranz/fixture".into(),
            config: Default::default(),
        },
    };
    let mut state = reducer::fold(&[first]).unwrap();
    state.mission.status = kranz_engine::types::MissionStatus::Running;
    state.runs.insert("r-1".into(), serde_json::from_value(json!({
        "id":"r-1", "role":"worker", "sdkSessionId":"s-engine", "model":"fixture", "startedAt":at(), "tokens":kranz_engine::types::TokenUsage::default(), "transcriptPath":"runs/r-1.jsonl", "promptHash":"fixture"
    })).unwrap());
    let root = tempfile::tempdir().unwrap();
    let action = json!({"kind":"execute","rawInput":{"command":"printf fixture"}});
    let options = vec![json!({"optionId":"once","kind":"allow_once"})];
    let proposal = permission::Proposal {
        id: "p-1".into(),
        engine_session_id: "s-engine".into(),
        peer_session_id: "peer-session".into(),
        peer_request_id: json!(12),
        tool_call_id: "tool-1".into(),
        action_digest: permission::digest(&action).unwrap(),
        options_digest: permission::digest(&options).unwrap(),
        action,
        options,
        observed_at: at(),
        deadline: at() + Duration::minutes(5),
        prohibition: None,
    };
    let binding = permission::Binding {
        mission_id: "m-lifecycle".into(),
        run_id: "r-1".into(),
        workspace: root.path().display().to_string(),
        plan_digest: permission::digest(&json!({"plan":"approved"})).unwrap(),
        policy_digest: permission::digest(&json!({"policy":"narrow"})).unwrap(),
    };
    let live = permission::Request::new(proposal, binding).unwrap();
    reducer::apply(
        &mut state,
        &Event {
            seq: 2,
            ts: at(),
            mission_id: "m-lifecycle".into(),
            kind: EventKind::PermissionRequested {
                request: live.clone(),
            },
        },
    )
    .unwrap();
    let mut request = requested("command-permission");
    request.permission_request_id = Some(live.proposal.id.clone());
    request.policy.mission_policy_digest =
        Digest::try_from(format!("sha256:{}", live.binding.policy_digest)).unwrap();
    request.request.params.binding.policy_digest = request.policy.digest();
    request.request.params.binding.plan_digest =
        Digest::try_from(format!("sha256:{}", live.binding.plan_digest)).unwrap();
    request.request.params.binding.workspace_id = workspace_id(&live.binding.workspace);
    request.request.params.subject = Subject::Invocation {
        run_id: Id::try_from("r-1".to_string()).unwrap(),
        peer_session_id: live.proposal.peer_session_id.clone(),
        tool_call_id: live.proposal.tool_call_id.clone(),
        peer_request_id: PeerRequestId::Number(12),
        action_digest: Digest::try_from(format!("sha256:{}", live.proposal.action_digest)).unwrap(),
        options_digest: Digest::try_from(format!("sha256:{}", live.proposal.options_digest))
            .unwrap(),
        cwd_id: workspace_id(&live.binding.workspace),
    };
    request.request.params.binding.subject_digest =
        Digest::of(&serde_json::to_vec(&request.request.params.subject).unwrap());
    let requested_event = Event {
        seq: 3,
        ts: at(),
        mission_id: "m-lifecycle".into(),
        kind: EventKind::GateEvaluationRequested {
            evaluation: Box::new(request.clone()),
        },
    };
    for change in ["workspace", "policy", "run-ended", "closed"] {
        let mut changed = state.clone();
        let permission = changed.permissions.get_mut("p-1").unwrap();
        match change {
            "workspace" => permission.request.binding.workspace.push_str("/other"),
            "policy" => permission.request.binding.policy_digest = "00".repeat(32),
            "run-ended" => changed.runs.get_mut("r-1").unwrap().ended_at = Some(at()),
            _ => permission.closed = Some("ended".into()),
        }
        assert!(
            reducer::apply(&mut changed, &requested_event).is_err(),
            "{change}"
        );
    }
    reducer::apply(&mut state, &requested_event).unwrap();
    reducer::apply(
        &mut state,
        &Event {
            seq: 4,
            ts: at(),
            mission_id: "m-lifecycle".into(),
            kind: EventKind::GateEvaluationFinished {
                evaluation: Box::new(finished(&request, Status::Judged, Some(Verdict::Pass))),
            },
        },
    )
    .unwrap();
    reducer::apply(
        &mut state,
        &Event {
            seq: 5,
            ts: at(),
            mission_id: "m-lifecycle".into(),
            kind: EventKind::PermissionResolved {
                resolution: permission::Resolution {
                    request_id: "p-1".into(),
                    binding_digest: live.binding_digest.clone(),
                    allow: true,
                    actor: Actor::LocalRepositoryAuthority,
                    reason: "operator allowed this invocation".into(),
                },
            },
        },
    )
    .unwrap();
    let record = &state.gate_evaluations[request.request.params.attempt_id.as_str()];
    let wrong = resolution(
        record,
        Some(Consent {
            actor: Actor::SlackUser("U-claimed".into()),
            allow: true,
            reference: "p-1".into(),
        }),
    );
    let mut forged = state.clone();
    assert!(reducer::apply(
        &mut forged,
        &Event {
            seq: 6,
            ts: at(),
            mission_id: "m-lifecycle".into(),
            kind: EventKind::GateResolutionRecorded {
                resolution: wrong.clone(),
            },
        },
    )
    .is_err());
    let consume = |r: &Resolution| Event {
        seq: 7,
        ts: at(),
        mission_id: "m-lifecycle".into(),
        kind: EventKind::GateResolutionConsumed {
            consumption: Consumed {
                attempt_id: r.attempt_id.clone(),
                resolution_id: r.id.clone(),
                rechecked_binding: r.binding.clone(),
                action: Action::AnswerPermission,
            },
        },
    };
    assert!(
        forged.gate_evaluations[request.request.params.attempt_id.as_str()]
            .resolution
            .is_none()
    );
    let actual = resolution(
        record,
        Some(Consent {
            actor: Actor::LocalRepositoryAuthority,
            allow: true,
            reference: "p-1".into(),
        }),
    );
    reducer::apply(
        &mut state,
        &Event {
            seq: 6,
            ts: at(),
            mission_id: "m-lifecycle".into(),
            kind: EventKind::GateResolutionRecorded {
                resolution: actual.clone(),
            },
        },
    )
    .unwrap();
    reducer::apply(&mut state, &consume(&actual)).unwrap();
    assert_eq!(state.consumed_gate_resolutions.len(), 1);
    assert!(
        state.permissions["p-1"].delivery.is_none(),
        "replay never sends the answer"
    );
}

#[test]
fn gate_lifecycle_closed_attempt_cannot_resolve_or_consume_after_recovery() {
    let request = requested("plan-approval");
    let finish = finished(&request, Status::Judged, Some(Verdict::Pass));
    let mut interrupted = Record::new(request.clone(), at(), 2).unwrap();
    interrupted.close("engine restarted".into()).unwrap();
    assert!(interrupted.finish(finish.clone(), at()).is_err());
    assert!(interrupted.close("again".into()).is_err());

    let mut resolved = Record::new(request, at(), 2).unwrap();
    resolved.finish(finish, at()).unwrap();
    let decision = resolution(&resolved, Some(consent()));
    resolved.resolve(decision.clone(), at()).unwrap();
    resolved
        .close("engine restarted before effect".into())
        .unwrap();
    assert_eq!(
        resolved.disposition(Some(&consent())).unwrap(),
        Disposition::Block
    );
    assert!(resolved
        .consume(
            Consumed {
                attempt_id: decision.attempt_id,
                resolution_id: decision.id,
                rechecked_binding: decision.binding,
                action: Action::ApprovePlan,
            },
            at()
        )
        .is_err());
}

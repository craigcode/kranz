use super::*;
use crate::backend_mock::{MockBackend, MockScript};
use crate::reviewer_independence::{check_completion, check_dispatch, model_family};

fn orchestrator_replies(replies: &[&str]) -> MockScript {
    use crate::backend_mock::{mock_init, mock_result_text, mock_text};
    MockScript::streaming(vec![mock_init("orch"), mock_result_text("ready")]).responding(
        replies
            .iter()
            .map(|reply| vec![mock_text(reply), mock_result_text(reply)])
            .collect(),
    )
}

fn deliver_feature(engine: &mut MissionEngine, backend: BackendKind, model: &str) {
    record_worker(engine, "worker-1", Some(backend), model);
    let path = engine.active_root().join("delivered.txt");
    std::fs::write(&path, "a real deliverable\n").unwrap();
    let commit = engine
        .active_repo()
        .commit_paths(&[&path], "[f-1-1] deliver feature")
        .unwrap();
    engine
        .emit(EventKind::WorkerCompleted {
            run_id: "worker-1".into(),
            result: RunResult::Pass,
            tokens: TokenUsage::default(),
            cost_usd: None,
            report: None,
        })
        .unwrap();
    engine
        .emit(EventKind::FeatureCompleted {
            feature_id: "f-1-1".into(),
            commits: vec![commit],
        })
        .unwrap();
}

fn completion_result(engine: &MissionEngine) -> std::result::Result<(), String> {
    let events = EventLog::read_events(&engine.paths.events_file()).unwrap();
    check_completion(&engine.state, &events, None).map_err(|blocked| blocked.detail)
}

fn policy_cfg() -> MissionConfig {
    let mut cfg = MissionConfig::default();
    cfg.worker.backend = Some("codex".into());
    cfg.skip_functional = true;
    cfg.reviewer_independence.scrutiny = true;
    // Mock sessions do not execute child code. Permit the existing loud
    // containment degrade on hosts without a supported sandbox.
    cfg.validator_allow_uncontained_degrade = true;
    cfg
}

fn plan() -> Plan {
    serde_json::from_value(serde_json::json!({
        "goal": "review policy test", "validationContract": [],
        "milestones": [{"title": "one", "features": [{
            "title": "change", "spec": "change", "validationCriteria": []
        }]}]
    }))
    .unwrap()
}

fn engine(
    cfg: MissionConfig,
    scripts: Vec<MockScript>,
) -> Option<(tempfile::TempDir, Arc<MockBackend>, MissionEngine)> {
    engine_with_plan(cfg, scripts, plan())
}

fn engine_with_plan(
    mut cfg: MissionConfig,
    scripts: Vec<MockScript>,
    plan: Plan,
) -> Option<(tempfile::TempDir, Arc<MockBackend>, MissionEngine)> {
    let (dir, root) = super::tests::lessons_test_repo()?;
    // These tests call lifecycle methods directly instead of run(), which
    // normally provisions the integration worktree before validation.
    cfg.worker_isolation = WorkerIsolation::Checkout;
    let mock = Arc::new(MockBackend::with_scripts(scripts));
    let mut engine = MissionEngine::create(mock.clone(), root, "review policy test", cfg).unwrap();
    engine.approve_plan(plan).unwrap();
    engine
        .emit(EventKind::MilestoneStarted {
            milestone_id: "ms-1".into(),
            start_sha: engine.repo.head_sha().unwrap(),
        })
        .unwrap();
    Some((dir, mock, engine))
}

fn record_worker(engine: &mut MissionEngine, id: &str, backend: Option<BackendKind>, model: &str) {
    engine
        .emit(EventKind::WorkerSpawned {
            run_id: id.into(),
            role: Role::Worker,
            feature_id: Some("f-1-1".into()),
            milestone_id: None,
            candidate: None,
            executor_route: None,
            sdk_session_id: id.into(),
            model: model.into(),
            backend,
            quant: "n/a".into(),
            weight_hash: None,
            prompt_hash: "test".into(),
            transcript_path: format!("runs/{id}.jsonl"),
        })
        .unwrap();
}

fn assert_blocked_without_reviewer(engine: &MissionEngine, mock: &MockBackend, reason: &str) {
    assert!(mock.started_specs().is_empty());
    assert_eq!(engine.state.mission.status, MissionStatus::Blocked);
    let events = EventLog::read_events(&engine.paths.events_file()).unwrap();
    assert!(events.iter().any(|event| matches!(&event.kind,
        EventKind::MilestoneBlocked { reason: detail, .. }
            if detail.contains("reviewer independence blocked") && detail.contains(reason)
    )));
    assert!(!events
        .iter()
        .any(|event| matches!(event.kind, EventKind::MilestoneCompleted { .. })));
}

#[test]
fn reviewer_independence_model_families_do_not_follow_cli_or_version() {
    for (backend, model) in [
        (BackendKind::Claude, "opus"),
        (BackendKind::Claude, "claude-sonnet-5"),
        (BackendKind::Droid, "claude-fable-5"),
        (BackendKind::Cursor, "claude-opus-4.6"),
    ] {
        assert_eq!(model_family(backend, model), Some("anthropic-claude"));
    }
    assert_eq!(
        model_family(BackendKind::Codex, "gpt-5.6-sol"),
        model_family(BackendKind::Cursor, "gpt-5")
    );
    for (backend, model) in [
        (BackendKind::Cursor, "auto"),
        (BackendKind::Droid, "custom:unknown"),
        (BackendKind::Local, "gpt-5"),
        (BackendKind::Acp, "claude-opus-4.6"),
        (BackendKind::Claude, "not-really-opus"),
    ] {
        assert_eq!(model_family(backend, model), None);
    }
}

#[test]
fn reviewer_independence_config_rejects_skips_same_family_and_unknown_pool_members() {
    let cfg = policy_cfg();
    config::validate(&cfg).unwrap();
    let mut skipped = cfg.clone();
    skipped.skip_scrutiny = true;
    assert!(config::validate(&skipped)
        .unwrap_err()
        .to_string()
        .contains("skip flag"));
    let mut same = cfg.clone();
    same.worker.backend = Some("droid".into());
    same.worker.model = "claude-fable-5".into();
    assert!(config::validate(&same)
        .unwrap_err()
        .to_string()
        .contains("share family"));
    let mut pool = cfg.clone();
    pool.worker_candidates = vec![
        CandidateSpec {
            backend: "codex".into(),
            model: "gpt-5".into(),
        },
        CandidateSpec {
            backend: "cursor".into(),
            model: "auto".into(),
        },
    ];
    assert!(config::validate(&pool)
        .unwrap_err()
        .to_string()
        .contains("unknown"));
    pool.worker_candidates[1] = CandidateSpec {
        backend: "claude".into(),
        model: "sonnet".into(),
    };
    assert!(config::validate(&pool)
        .unwrap_err()
        .to_string()
        .contains("share family"));
    for source in [config::PatchSource::Inbox, config::PatchSource::Operator] {
        assert!(config::apply_validated_patch_from(
            &cfg,
            &serde_json::json!({"reviewerIndependence": {"scrutiny": false}}),
            source
        )
        .is_err());
    }
    assert!(serde_json::from_value::<MissionConfig>(serde_json::json!({
        "reviewerIndependence": {"scruntiny": true}
    }))
    .is_err());
}

#[tokio::test]
async fn reviewer_independence_unavailable_reviewer_blocks_before_spawn() {
    let mut cfg = policy_cfg();
    cfg.worker.backend = None;
    cfg.validator_scrutiny.backend = Some("droid".into());
    let Some((_dir, mock, mut engine)) = engine(cfg, vec![]) else {
        return;
    };
    record_worker(&mut engine, "worker-1", Some(BackendKind::Claude), "sonnet");
    let _guard = crate::preflight::DroidEnvGuard::engage();
    engine.validation_round(0).await.unwrap();
    assert_blocked_without_reviewer(&engine, &mock, "same family");
}

#[tokio::test]
async fn reviewer_independence_worker_fallback_records_actual_identity() {
    let mut cfg = policy_cfg();
    cfg.worker.backend = Some("droid".into());
    cfg.allow_below_default_worker_model = true;
    let Some((_dir, mock, mut engine)) =
        engine(cfg, vec![MockScript::single_shot("worker output")])
    else {
        return;
    };
    let _guard = crate::preflight::DroidEnvGuard::engage();
    let selected = engine.select_backend(Role::Worker);
    assert_eq!(selected.kind, BackendKind::Claude);
    assert_eq!(selected.cfg.worker.backend.as_deref(), Some("claude"));
    assert_eq!(selected.cfg.worker.model, "sonnet");
    let feature = engine.state.mission.milestones[0].features[0].clone();
    runner::run_worker(
        selected.backend.as_ref(),
        &mut engine.log,
        &engine.paths,
        &selected.cfg,
        &feature,
        "goal",
        "milestone",
        None,
        None,
        None,
        &[],
        &[],
        &[],
        AuthVerdict::Authenticated,
        &[],
        None,
        None,
    )
    .await
    .unwrap();
    engine.catch_up().unwrap();
    let worker = engine
        .state
        .runs
        .values()
        .find(|run| run.role == Role::Worker)
        .unwrap();
    assert_eq!(worker.backend, Some(BackendKind::Claude));
    assert_eq!(worker.model, "sonnet");
    let events = EventLog::read_events(&engine.paths.events_file()).unwrap();
    let chain = crate::provenance::provenance_chain(
        &engine.paths.mission_dir(),
        &engine.state.mission.id,
        &events,
    )
    .unwrap();
    assert_eq!(
        chain
            .sessions
            .iter()
            .find(|run| run.role == Role::Worker)
            .unwrap()
            .backend
            .as_deref(),
        Some("claude")
    );
    engine.validation_round(0).await.unwrap();
    assert_eq!(mock.started_specs().len(), 1, "only the worker may launch");
    assert_eq!(engine.state.mission.status, MissionStatus::Blocked);
}

#[tokio::test]
async fn reviewer_independence_fallback_can_run_if_family_remains_distinct() {
    let mut cfg = policy_cfg();
    cfg.validator_scrutiny.backend = Some("droid".into());
    cfg.validator_scrutiny.model = crate::cost::DEFAULT_DROID_MODEL.into();
    let scripts = vec![MockScript::single_shot_json(&serde_json::json!({
        "findings": [], "summary": "clean"
    }))];
    let Some((_dir, mock, mut engine)) = engine(cfg, scripts) else {
        return;
    };
    record_worker(&mut engine, "worker-1", Some(BackendKind::Codex), "gpt-5");
    let _guard = crate::preflight::DroidEnvGuard::engage();
    engine.validation_round(0).await.unwrap();
    assert_eq!(mock.started_specs().len(), 1);
    assert_eq!(mock.started_specs()[0].model, "opus");
    assert_eq!(
        engine.state.mission.milestones[0].status,
        MilestoneStatus::Complete
    );
}

#[tokio::test]
async fn reviewer_independence_cross_family_retry_passes_and_records_both_checks() {
    let scripts = vec![
        MockScript::single_shot("no report"),
        MockScript::single_shot_json(&serde_json::json!({"findings": [], "summary": "clean"})),
    ];
    let Some((_dir, mock, mut engine)) = engine(policy_cfg(), scripts) else {
        return;
    };
    record_worker(
        &mut engine,
        "worker-1",
        Some(BackendKind::Codex),
        "gpt-5.6-sol",
    );
    engine.validation_round(0).await.unwrap();
    assert_eq!(mock.started_specs().len(), 2);
    assert!(engine
        .state
        .runs
        .values()
        .filter(|run| run.role == Role::ValidatorScrutiny)
        .all(|run| run.backend == Some(BackendKind::Claude)));
    assert_eq!(
        engine.state.mission.milestones[0].status,
        MilestoneStatus::Complete
    );
    let events = EventLog::read_events(&engine.paths.events_file()).unwrap();
    assert_eq!(events.iter().filter(|event| matches!(&event.kind,
        EventKind::OrchestratorDecision { summary, .. } if summary == "reviewer independence satisfied"
    )).count(), 2);
}

#[tokio::test]
async fn reviewer_independence_pin_survives_config_replay_and_disabled_role() {
    let Some((_dir, mock, mut engine)) = engine(policy_cfg(), vec![]) else {
        return;
    };
    record_worker(&mut engine, "worker-1", Some(BackendKind::Codex), "gpt-5");
    let pin = engine.state.mission.reviewer_independence;
    // Even a legacy/injected event that bypasses the runtime patch allowlist
    // cannot erase the separately folded approval pin.
    engine
        .emit(EventKind::ConfigChanged {
            patch: serde_json::json!({
                "reviewerIndependence": {"scrutiny": false}, "skipScrutiny": true
            }),
        })
        .unwrap();
    let id = engine.state.mission.id.clone();
    let root = engine.paths.repo_root.clone();
    drop(engine);
    let mut resumed = MissionEngine::resume(mock.clone(), &root, &id, LockForce::No).unwrap();
    assert_eq!(resumed.state.mission.reviewer_independence, pin);
    resumed.validation_round(0).await.unwrap();
    assert_blocked_without_reviewer(&resumed, &mock, "disabled");
    let mut revised = plan();
    assert!(reducer::dry_run_revised_plan(&resumed.state, &revised, 1).is_err());
    revised.reviewer_independence = pin;
    reducer::dry_run_revised_plan(&resumed.state, &revised, 1).unwrap();
}

#[test]
fn reviewer_independence_prior_attempts_and_missing_provenance_cannot_be_erased() {
    let Some((_dir, _mock, mut engine)) = engine(policy_cfg(), vec![]) else {
        return;
    };
    let check = |engine: &MissionEngine| {
        check_dispatch(
            &engine.state,
            Role::ValidatorScrutiny,
            BackendKind::Claude,
            "opus",
        )
    };
    assert!(check(&engine).unwrap_err().contains("no recorded"));
    record_worker(&mut engine, "old", None, "gpt-5");
    assert!(check(&engine).unwrap_err().contains("unknown"));
    engine.state.runs.remove("old");
    record_worker(&mut engine, "first", Some(BackendKind::Claude), "sonnet");
    record_worker(&mut engine, "repair", Some(BackendKind::Codex), "gpt-5");
    assert!(check(&engine).unwrap_err().contains("first"));
}

#[tokio::test]
async fn reviewer_independence_confirmation_cannot_collapse_to_worker_family() {
    let mut cfg = policy_cfg();
    cfg.worker.backend = None;
    cfg.reviewer_independence = ReviewerIndependence {
        scrutiny: false,
        functional: true,
    };
    cfg.skip_functional = false;
    cfg.skip_scrutiny = true;
    cfg.validator_functional.backend = Some("codex".into());
    let Some((_dir, mock, mut engine)) = engine(cfg, vec![]) else {
        return;
    };
    record_worker(&mut engine, "worker-1", Some(BackendKind::Claude), "sonnet");
    let milestone = engine.state.mission.milestones[0].clone();
    let report: ValidatorReport = serde_json::from_value(serde_json::json!({
        "findings": [], "summary": "local pass"
    }))
    .unwrap();
    let result = engine
        .confirm_local_functional_pass(
            "ms-1",
            Role::ValidatorFunctional,
            &milestone,
            &[],
            "HEAD",
            None,
            &[],
            &[],
            &[],
            None,
            None,
            "local-1",
            &report,
            &[],
        )
        .await
        .unwrap();
    assert!(result.is_none());
    assert_blocked_without_reviewer(&engine, &mock, "same family");
}

#[test]
fn reviewer_independence_approval_rejects_substitution_and_persists_consent() {
    let cfg = policy_cfg();
    let (Some((_dir, _mock, engine)), mut forged) = (engine(cfg.clone(), vec![]), plan()) else {
        return;
    };
    let pin = Some(cfg.reviewer_independence);
    assert_eq!(engine.state.mission.reviewer_independence, pin);
    let saved = engine
        .repo
        .show_file(
            &engine.state.mission.mission_branch,
            &format!(".kranz/missions/{}/plan.json", engine.state.mission.id),
        )
        .unwrap();
    let saved: Plan = serde_json::from_slice(&saved.unwrap()).unwrap();
    assert_eq!(saved.reviewer_independence, pin);
    forged.reviewer_independence = Some(ReviewerIndependence::default());
    assert!(crate::reviewer_independence::pin_plan(&mut forged, pin).is_err());
    assert!(plan().reviewer_independence.is_none());
    assert!(serde_json::to_value(MissionConfig::default())
        .unwrap()
        .get("reviewerIndependence")
        .is_none());
}

#[tokio::test]
async fn reviewer_independence_steered_skip_and_replayed_completion_cannot_bypass_review() {
    let scripts = vec![orchestrator_replies(&[
        r#"{"action":"skip-milestone","note":"ship the retained work"}"#,
    ])];
    let Some((_dir, mock, mut engine)) = engine(policy_cfg(), scripts) else {
        return;
    };
    // Actual worker identity diverged from the approved configuration. Its
    // real commit must survive so the empty-deliverable gate cannot mask this.
    deliver_feature(&mut engine, BackendKind::Claude, "sonnet");
    engine.validation_round(0).await.unwrap();
    assert_blocked_without_reviewer(&engine, &mock, "same family");
    let commits = engine.state.mission.milestones[0].features[0]
        .commits
        .clone();

    control::enqueue(
        &engine.paths,
        &ControlCommand::Msg {
            text: "skip this milestone and ship the completed feature".into(),
            interrupt: false,
        },
    )
    .unwrap();
    engine.drain_control().await.unwrap();
    assert_eq!(
        engine.handle_blocked(0).await.unwrap(),
        Some(MissionStatus::Blocked)
    );
    assert_eq!(mock.started_specs().len(), 1, "only the orchestrator ran");
    assert_eq!(
        engine.state.mission.milestones[0].features[0].commits,
        commits
    );
    let events = EventLog::read_events(&engine.paths.events_file()).unwrap();
    assert!(!events.iter().any(|event| matches!(
        event.kind,
        EventKind::MilestoneCompleted { .. } | EventKind::FeatureSkipped { .. }
    )));

    // An old engine could already have emitted this completion. Replay it
    // normally, then prove that the final gate independently rejects it.
    engine
        .emit(EventKind::MilestoneCompleted {
            milestone_id: "ms-1".into(),
            tag: None,
        })
        .unwrap();
    let id = engine.state.mission.id.clone();
    let root = engine.paths.repo_root.clone();
    drop(engine);
    let mut resumed = MissionEngine::resume(mock.clone(), &root, &id, LockForce::No).unwrap();
    assert_eq!(first_incomplete(&resumed.state), None);
    assert_eq!(
        resumed.final_gate().await.unwrap(),
        Some(MissionStatus::Blocked)
    );
    assert_eq!(
        resumed.complete_mission(None).await.unwrap(),
        MissionStatus::Blocked
    );
    assert_eq!(mock.started_specs().len(), 1, "no completion-model turn");
    let events = EventLog::read_events(&resumed.paths.events_file()).unwrap();
    assert!(!events.iter().any(|event| matches!(
        event.kind,
        EventKind::MissionCompleted { .. } | EventKind::MissionFailed { .. }
    )));
}

#[tokio::test]
async fn reviewer_independence_complete_requires_both_successful_roles_and_survives_replay() {
    let mut cfg = policy_cfg();
    cfg.skip_functional = false;
    cfg.reviewer_independence.functional = true;
    let clean = serde_json::json!({"findings": [], "summary": "reviewed"});
    let scripts = vec![
        MockScript::single_shot_json(&clean),
        MockScript::single_shot_json(&clean),
        orchestrator_replies(&["NONE"]),
    ];
    let Some((_dir, mock, mut engine)) = engine(cfg, scripts) else {
        return;
    };
    deliver_feature(&mut engine, BackendKind::Codex, "gpt-5");
    engine.validation_round(0).await.unwrap();
    completion_result(&engine).unwrap();
    assert_eq!(mock.started_specs().len(), 2);
    let events = EventLog::read_events(&engine.paths.events_file()).unwrap();
    for role in [Role::ValidatorScrutiny, Role::ValidatorFunctional] {
        let mut state = engine.state.clone();
        state.runs.retain(|_, run| run.role != role);
        assert!(check_completion(&state, &events, None)
            .unwrap_err()
            .detail
            .contains("no successful review"));
    }

    let id = engine.state.mission.id.clone();
    let root = engine.paths.repo_root.clone();
    drop(engine);
    let mut resumed = MissionEngine::resume(mock.clone(), &root, &id, LockForce::No).unwrap();
    completion_result(&resumed).unwrap();
    assert_eq!(
        resumed.final_gate().await.unwrap(),
        Some(MissionStatus::Complete)
    );
    assert_eq!(resumed.state.mission.status, MissionStatus::Complete);
}

#[tokio::test]
async fn reviewer_independence_review_cannot_be_reused_after_repair_or_failed_round() {
    let clean = serde_json::json!({"findings": [], "summary": "reviewed"});
    let scripts = vec![MockScript::single_shot_json(&clean)];
    let Some((_dir, _mock, mut engine)) = engine(policy_cfg(), scripts) else {
        return;
    };
    deliver_feature(&mut engine, BackendKind::Codex, "gpt-5");
    engine.validation_round(0).await.unwrap();
    completion_result(&engine).unwrap();
    record_worker(&mut engine, "repair", Some(BackendKind::Codex), "gpt-5");
    assert!(completion_result(&engine).unwrap_err().contains("stale"));
    engine
        .emit(EventKind::MilestoneValidating {
            milestone_id: "ms-1".into(),
        })
        .unwrap();
    assert!(completion_result(&engine)
        .unwrap_err()
        .contains("no successful review"));
}

#[tokio::test]
async fn reviewer_independence_failed_or_tampered_review_is_not_completion_evidence() {
    let clean = serde_json::json!({"findings": [], "summary": "reviewed"});
    let Some((_dir, _mock, mut engine)) =
        engine(policy_cfg(), vec![MockScript::single_shot_json(&clean)])
    else {
        return;
    };
    deliver_feature(&mut engine, BackendKind::Codex, "gpt-5");
    engine.validation_round(0).await.unwrap();
    let reviewer = engine
        .state
        .runs
        .values()
        .find(|run| run.role == Role::ValidatorScrutiny)
        .unwrap()
        .id
        .clone();
    let events = EventLog::read_events(&engine.paths.events_file()).unwrap();
    for result in [None, Some(RunResult::Fail), Some(RunResult::Partial)] {
        let mut state = engine.state.clone();
        state.runs.get_mut(&reviewer).unwrap().result = result;
        assert!(check_completion(&state, &events, None).is_err());
    }
    engine
        .emit(EventKind::ValidatorTamper {
            milestone_id: "ms-1".into(),
            run_id: reviewer,
            role: Role::ValidatorScrutiny,
            head_before: engine.repo.head_sha().unwrap(),
            head_after: engine.repo.head_sha().unwrap(),
            appeared: vec![" M delivered.txt".into()],
            resolved: vec![],
            git_metadata_changed: false,
            git_metadata_fields: vec![],
        })
        .unwrap();
    assert!(completion_result(&engine).unwrap_err().contains("stale"));
}

#[tokio::test]
async fn reviewer_independence_legacy_no_policy_skip_keeps_existing_completion_behavior() {
    let cfg = MissionConfig {
        skip_scrutiny: true,
        skip_functional: true,
        ..MissionConfig::default()
    };
    let scripts = vec![orchestrator_replies(&[
        r#"{"action":"skip-milestone","note":"skip the remaining work"}"#,
        "NONE",
    ])];
    let Some((_dir, _mock, mut engine)) = engine(cfg, scripts) else {
        return;
    };
    deliver_feature(&mut engine, BackendKind::Claude, "sonnet");
    engine
        .emit(EventKind::MilestoneBlocked {
            block_context: None,
            milestone_id: "ms-1".into(),
            reason: "operator decision needed".into(),
        })
        .unwrap();
    engine
        .emit(EventKind::UserMessage {
            text: "skip this milestone".into(),
            interrupt: false,
        })
        .unwrap();
    assert_eq!(engine.handle_blocked(0).await.unwrap(), None);
    assert_eq!(first_incomplete(&engine.state), None);
    assert_eq!(
        engine.final_gate().await.unwrap(),
        Some(MissionStatus::Complete)
    );
}

#[cfg(unix)]
#[tokio::test]
async fn reviewer_independence_final_command_mutation_retires_review_across_resume() {
    for commit in [false, true] {
        let mut approved = plan();
        let mut command = "printf 'changed by final gate\\n' > delivered.txt".to_string();
        if commit {
            command.push_str(" && git add delivered.txt && git -c user.name=test -c user.email=test@invalid -c commit.gpgSign=false commit -m 'final gate edit'");
        }
        approved.validation_contract.push(Assertion {
            id: "a1".into(),
            statement: "exercise a mutating gate".into(),
            check: AssertionCheck::Command,
            command: Some(command),
            negative_control: None,
            pty_script: None,
        });
        let scripts = vec![MockScript::single_shot_json(&serde_json::json!({
            "findings": [], "summary": "independent review before final gate"
        }))];
        let Some((_dir, mock, mut engine)) = engine_with_plan(policy_cfg(), scripts, approved)
        else {
            return;
        };
        deliver_feature(&mut engine, BackendKind::Codex, "gpt-5");
        engine.validation_round(0).await.unwrap();
        completion_result(&engine).unwrap();
        assert_eq!(
            engine.final_gate().await.unwrap(),
            Some(MissionStatus::Blocked)
        );
        assert_eq!(
            std::fs::read_to_string(engine.paths.repo_root.join("delivered.txt")).unwrap(),
            "changed by final gate\n"
        );
        assert!(completion_result(&engine)
            .unwrap_err()
            .contains("no successful review"));
        let id = engine.state.mission.id.clone();
        let root = engine.paths.repo_root.clone();
        drop(engine);
        let mut resumed = MissionEngine::resume(mock.clone(), &root, &id, LockForce::No).unwrap();
        assert_eq!(
            resumed.final_gate().await.unwrap(),
            Some(MissionStatus::Blocked)
        );
        assert!(completion_result(&resumed).is_err());
        assert_eq!(mock.started_specs().len(), 1, "no completion turn may run");
        let events = EventLog::read_events(&resumed.paths.events_file()).unwrap();
        assert!(!events
            .iter()
            .any(|event| matches!(event.kind, EventKind::MissionCompleted {})));
    }
}

#[tokio::test]
async fn reviewer_independence_final_gate_refuses_an_already_dirty_review_baseline() {
    let scripts = vec![MockScript::single_shot_json(&serde_json::json!({
        "findings": [], "summary": "independent review"
    }))];
    let Some((_dir, _mock, mut engine)) = engine(policy_cfg(), scripts) else {
        return;
    };
    deliver_feature(&mut engine, BackendKind::Codex, "gpt-5");
    // The snapshot may legitimately review a dirty tree, but completion
    // requires a checkpoint identity before any final gate can mutate it.
    std::fs::write(
        engine.paths.repo_root.join("delivered.txt"),
        "reviewed dirty bytes\n",
    )
    .unwrap();
    engine.validation_round(0).await.unwrap();
    completion_result(&engine).unwrap();
    assert_eq!(
        engine.final_gate().await.unwrap(),
        Some(MissionStatus::Blocked)
    );
    assert!(completion_result(&engine)
        .unwrap_err()
        .contains("no successful review"));
}

#[tokio::test]
async fn reviewer_independence_pending_only_revision_preserves_completed_prefix_review() {
    let mut approved = plan();
    approved.milestones.push(PlanMilestone {
        title: "two".into(),
        features: vec![PlanFeature {
            title: "second change".into(),
            spec: "initial pending specification".into(),
            validation_criteria: vec![],
        }],
    });
    let clean = serde_json::json!({"findings": [], "summary": "independent review"});
    let scripts = vec![
        MockScript::single_shot_json(&clean),
        MockScript::single_shot_json(&clean),
        orchestrator_replies(&["NONE"]),
    ];
    let Some((_dir, mock, mut engine)) = engine_with_plan(policy_cfg(), scripts, approved.clone())
    else {
        return;
    };
    deliver_feature(&mut engine, BackendKind::Codex, "gpt-5");
    engine.validation_round(0).await.unwrap();
    approved.reviewer_independence = engine.state.mission.reviewer_independence;
    approved.milestones[1].features[0].spec = "revised pending specification".into();
    engine
        .emit(EventKind::PlanRevised {
            revision: 1,
            plan: approved.clone(),
        })
        .unwrap();
    let events = EventLog::read_events(&engine.paths.events_file()).unwrap();
    check_completion(&engine.state, &events, Some("ms-1")).unwrap();
    engine
        .emit(EventKind::MilestoneStarted {
            milestone_id: "ms-2".into(),
            start_sha: engine.repo.head_sha().unwrap(),
        })
        .unwrap();
    let mut spawned = events
        .iter()
        .find(|event| {
            matches!(
                event.kind,
                EventKind::WorkerSpawned {
                    role: Role::Worker,
                    ..
                }
            )
        })
        .unwrap()
        .kind
        .clone();
    if let EventKind::WorkerSpawned {
        run_id, feature_id, ..
    } = &mut spawned
    {
        *run_id = "worker-2".into();
        *feature_id = Some("f-2-1".into());
    }
    engine.emit(spawned).unwrap();
    let second_path = engine.paths.repo_root.join("second.txt");
    std::fs::write(&second_path, "second deliverable\n").unwrap();
    let second_commit = engine
        .repo
        .commit_paths(&[&second_path], "[f-2-1] second change")
        .unwrap();
    engine
        .emit(EventKind::WorkerCompleted {
            run_id: "worker-2".into(),
            result: RunResult::Pass,
            tokens: TokenUsage::default(),
            cost_usd: None,
            report: None,
        })
        .unwrap();
    engine
        .emit(EventKind::FeatureCompleted {
            feature_id: "f-2-1".into(),
            commits: vec![second_commit],
        })
        .unwrap();
    engine.validation_round(1).await.unwrap();
    completion_result(&engine).unwrap();

    // A changed global review contract DOES invalidate the earlier evidence.
    let mut changed_contract = approved;
    changed_contract.validation_contract.push(Assertion {
        id: "a1".into(),
        statement: "additional requirement".into(),
        check: AssertionCheck::Command,
        command: Some("true".into()),
        negative_control: None,
        pty_script: None,
    });
    let mut revised_events = EventLog::read_events(&engine.paths.events_file()).unwrap();
    let last = revised_events.last().unwrap();
    revised_events.push(Event {
        seq: last.seq + 1,
        ts: last.ts,
        mission_id: last.mission_id.clone(),
        kind: EventKind::PlanRevised {
            revision: 2,
            plan: changed_contract,
        },
    });
    let revised_state = reducer::fold(&revised_events).unwrap();
    assert!(
        check_completion(&revised_state, &revised_events, Some("ms-1"))
            .unwrap_err()
            .detail
            .contains("changed the reviewed milestone context")
    );

    let id = engine.state.mission.id.clone();
    let root = engine.paths.repo_root.clone();
    drop(engine);
    let mut resumed = MissionEngine::resume(mock, &root, &id, LockForce::No).unwrap();
    completion_result(&resumed).unwrap();
    assert_eq!(
        resumed.final_gate().await.unwrap(),
        Some(MissionStatus::Complete)
    );
}

#[tokio::test]
async fn reviewer_independence_worktree_finalization_preserves_primary_source() {
    let Some((_dir, root)) = super::tests::lessons_test_repo() else {
        return;
    };
    let mock = Arc::new(MockBackend::with_scripts(vec![
        MockScript::single_shot_json(&serde_json::json!({"findings": [], "summary": "clean"})),
        orchestrator_replies(&["Record independent review before completion."]),
    ]));
    let mut cfg = policy_cfg();
    cfg.worker_isolation = WorkerIsolation::Worktree;
    let mut engine = MissionEngine::create(mock, &root, "worktree review", cfg).unwrap();
    engine.approve_plan(plan()).unwrap();
    let primary_head = engine.repo.head_sha().unwrap();
    let primary_source = std::fs::read(root.join("README.md")).unwrap();
    engine.primary_branch_at_start = Some(engine.repo.current_branch().unwrap());
    engine.active_tree = Some(engine.setup_mission_worktree().unwrap());
    engine
        .emit(EventKind::MilestoneStarted {
            milestone_id: "ms-1".into(),
            start_sha: engine.active_repo().head_sha().unwrap(),
        })
        .unwrap();
    deliver_feature(&mut engine, BackendKind::Codex, "gpt-5");
    engine.validation_round(0).await.unwrap();
    completion_result(&engine).unwrap();
    assert_eq!(
        engine.final_gate().await.unwrap(),
        Some(MissionStatus::Complete)
    );
    assert!(engine.active_paths().report_file().exists());
    assert_eq!(engine.repo.head_sha().unwrap(), primary_head);
    assert_eq!(
        std::fs::read(root.join("README.md")).unwrap(),
        primary_source
    );
    assert!(!root.join("delivered.txt").exists());
    assert!(engine.repo.is_clean_tracked().unwrap());
    engine.teardown_mission_worktree();
    engine.active_tree = None;
}

#[tokio::test]
async fn reviewer_independence_lesson_turn_cannot_mutate_reviewed_work_before_completion() {
    let scripts = vec![
        MockScript::single_shot_json(&serde_json::json!({"findings": [], "summary": "clean"})),
        orchestrator_replies(&["NONE"]).writes_file("delivered.txt", "late mutation\n"),
    ];
    let Some((_dir, _mock, mut engine)) = engine(policy_cfg(), scripts) else {
        return;
    };
    deliver_feature(&mut engine, BackendKind::Codex, "gpt-5");
    engine.validation_round(0).await.unwrap();
    assert_eq!(
        engine.final_gate().await.unwrap(),
        Some(MissionStatus::Blocked)
    );
    assert!(completion_result(&engine)
        .unwrap_err()
        .contains("no successful review"));
    assert!(!engine.active_paths().report_file().exists());
}

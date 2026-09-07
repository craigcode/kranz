use super::*;
use crate::backend_mock::{MockBackend, MockScript};
use crate::reviewer_independence::{check_dispatch, model_family};

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
    let (dir, root) = super::tests::lessons_test_repo()?;
    let mock = Arc::new(MockBackend::with_scripts(scripts));
    let mut engine = MissionEngine::create(mock.clone(), root, "review policy test", cfg).unwrap();
    engine.approve_plan(plan()).unwrap();
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

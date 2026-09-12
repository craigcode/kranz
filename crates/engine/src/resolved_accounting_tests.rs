use super::*;
use crate::types::{ExecutorTier, RunResult};

fn append(events: &mut Vec<Event>, kind: EventKind) {
    events.push(Event {
        seq: events.len() as u64 + 1,
        ts: chrono::DateTime::from_timestamp(events.len() as i64, 0).unwrap(),
        mission_id: "m-accounting".into(),
        kind,
    });
}

fn mission(config: MissionConfig) -> Vec<Event> {
    let mut events = Vec::new();
    append(
        &mut events,
        EventKind::MissionCreated {
            goal: "accounting regression".into(),
            base_branch: "main".into(),
            mission_branch: "kranz/accounting".into(),
            config,
        },
    );
    append(&mut events, EventKind::PlanApproved {
        plan: serde_json::from_value(serde_json::json!({
            "goal": "accounting regression", "validationContract": [],
            "milestones": [{"title": "delivery", "features": [{"title": "work", "spec": "deliver", "validationCriteria": []}]}]
        })).unwrap(),
        base_sha: None,
    });
    events
}

fn run(
    events: &mut Vec<Event>,
    backend: Option<BackendKind>,
    model: &str,
    cost_usd: Option<f64>,
) -> TokenUsage {
    let id = format!("r-{}", events.len());
    append(
        events,
        EventKind::WorkerSpawned {
            run_id: id.clone(),
            role: Role::Worker,
            feature_id: Some("f-1-1".into()),
            milestone_id: Some("ms-1".into()),
            candidate: None,
            executor_route: None,
            sdk_session_id: "s".into(),
            model: model.into(),
            backend,
            quant: "n/a".into(),
            weight_hash: None,
            prompt_hash: "h".into(),
            transcript_path: "t".into(),
        },
    );
    let tokens = TokenUsage {
        input: 100_000,
        output: 10_000,
        cache_read: 20_000,
        cache_write: 1_000,
    };
    append(
        events,
        EventKind::WorkerCompleted {
            run_id: id,
            result: RunResult::Pass,
            tokens: tokens.clone(),
            cost_usd,
            report: None,
        },
    );
    tokens
}

#[test]
fn resolved_run_accounting_mixed_backends_agree_across_outcomes_and_calibration() {
    let mut cfg = MissionConfig::default();
    cfg.worker.backend = Some("local".into());
    let mut events = mission(cfg);
    run(&mut events, Some(BackendKind::Local), "local-model", None);
    append(
        &mut events,
        EventKind::TierEscalated {
            milestone_id: "ms-1".into(),
            from: ExecutorTier::Local,
            to: ExecutorTier::Frontier,
            reason: "operator escalation".into(),
        },
    );
    let tokens = run(&mut events, Some(BackendKind::Claude), "sonnet", None);
    run(
        &mut events,
        Some(BackendKind::Codex),
        DEFAULT_CODEX_MODEL,
        Some(0.0),
    );
    run(
        &mut events,
        Some(BackendKind::Droid),
        DEFAULT_DROID_MODEL,
        Some(7.0),
    );
    let expected = usage_cost_usd(&tokens, "sonnet") + 7.0;
    let state = reducer::fold(&events).unwrap();
    let outcomes = crate::outcomes::mission_outcomes("m-accounting", &events);
    assert!(expected > 7.0);
    assert_eq!(outcomes.cost_usd, expected);
    assert_eq!(mission_total_cost(&state), expected);
    assert_eq!(mission_actuals(&state).avg_worker_run_usd, expected / 4.0);
    assert_eq!(outcomes.token_sums.len(), 4);
    for backend in [
        BackendKind::Local,
        BackendKind::Claude,
        BackendKind::Codex,
        BackendKind::Droid,
    ] {
        let sum = outcomes
            .token_sums
            .iter()
            .find(|sum| sum.backend == backend)
            .unwrap();
        assert_eq!(
            (sum.runs, sum.fresh_input, sum.cache_read),
            (1, 100_000, 20_000)
        );
    }
}

#[test]
fn resolved_run_accounting_provider_fallback_overrides_config_for_cache_and_cost() {
    let mut cfg = MissionConfig::default();
    cfg.worker.backend = Some("droid".into());
    let mut events = mission(cfg);
    let tokens = run(&mut events, Some(BackendKind::Claude), "sonnet", None);
    let outcomes = crate::outcomes::mission_outcomes("m-accounting", &events);
    assert_eq!(outcomes.token_sums[0].backend, BackendKind::Claude);
    assert_eq!(outcomes.cost_usd, usage_cost_usd(&tokens, "sonnet"));
}

#[test]
fn resolved_run_accounting_legacy_logs_keep_each_consumers_historical_fallback() {
    let mut cfg = MissionConfig::default();
    cfg.worker.backend = Some("local".into());
    let mut events = mission(cfg);
    let tokens = run(&mut events, None, "sonnet", None);
    append(
        &mut events,
        EventKind::TierEscalated {
            milestone_id: "ms-1".into(),
            from: ExecutorTier::Local,
            to: ExecutorTier::Frontier,
            reason: "legacy escalation".into(),
        },
    );
    // Round-trip truly old wire data: no invented backend evidence.
    let json = serde_json::to_string(&events).unwrap();
    assert!(!json.contains("\"backend\":null"));
    let events: Vec<Event> = serde_json::from_str(&json).unwrap();
    let state = reducer::fold(&events).unwrap();
    let outcomes = crate::outcomes::mission_outcomes("m-accounting", &events);
    assert_eq!(outcomes.token_sums[0].backend, BackendKind::Local);
    assert_eq!(outcomes.cost_usd, 0.0);
    assert_eq!(
        mission_total_cost(&state),
        usage_cost_usd(&tokens, "sonnet")
    );
    assert_eq!(
        mission_actuals(&state).avg_worker_run_usd,
        mission_total_cost(&state)
    );
    assert_eq!(
        resolved_run_backend(None, Role::Worker, None),
        BackendKind::Claude
    );
}

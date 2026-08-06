//! Integration tests for config layering, cost estimation (including
//! calibration from recorded actuals), and role prompts.

use chrono::Utc;
use kranz_engine::config;
use kranz_engine::cost::{self, EstimateParams};
use kranz_engine::error::EngineError;
use kranz_engine::events::{Event, EventKind};
use kranz_engine::prompts;
use kranz_engine::types::{
    Assertion, AssertionCheck, Feature, FeatureOrigin, FeatureStatus, Finding, MissionConfig, Plan,
    PlanFeature, PlanMilestone, Role, RunResult, TokenUsage,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

fn approx(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-9,
        "expected {expected}, got {actual}"
    );
}

/// Default config with one knob turned, without tripping
/// clippy::field_reassign_with_default.
fn cfg_with(tweak: impl FnOnce(&mut MissionConfig)) -> MissionConfig {
    let mut cfg = MissionConfig::default();
    tweak(&mut cfg);
    cfg
}

// ---------------------------------------------------------------------------
// config
// ---------------------------------------------------------------------------

#[test]
fn default_config_is_valid() {
    config::validate(&MissionConfig::default()).expect("default config must validate");
}

#[test]
fn layered_load_partial_files_override_only_their_keys() {
    let dir = tempfile::tempdir().unwrap();
    let global = dir.path().join("global-config.json");
    let project = dir.path().join("project-config.json");

    // Global layer: overrides worker.model and maxRespawns.
    std::fs::write(
        &global,
        r#"{ "worker": { "model": "haiku" }, "maxRespawns": 4 }"#,
    )
    .unwrap();
    // Project layer: overrides a *different* nested worker key, plus a flag,
    // plus a key the global layer also set (later layer must win).
    std::fs::write(
        &project,
        r#"{ "worker": { "reasoningEffort": "high" }, "skipFunctional": true, "maxRespawns": 5 }"#,
    )
    .unwrap();

    let cfg = config::load_layers(&[global, project]).unwrap();

    // From global (untouched by project):
    assert_eq!(cfg.worker.model, "haiku");
    // From project:
    assert_eq!(cfg.worker.reasoning_effort, "high");
    assert!(cfg.skip_functional);
    // Later layer wins on conflict:
    assert_eq!(cfg.max_respawns, 5);
    // Untouched keys keep their defaults:
    let default = MissionConfig::default();
    assert_eq!(cfg.worker.max_turns, default.worker.max_turns);
    assert_eq!(cfg.orchestrator, default.orchestrator);
    assert_eq!(
        cfg.max_fix_cycles_per_milestone,
        default.max_fix_cycles_per_milestone
    );
    assert!(!cfg.skip_scrutiny);
}

#[test]
fn load_missing_layers_and_unknown_keys_are_fine() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("does-not-exist.json");
    let with_unknown = dir.path().join("unknown-keys.json");
    std::fs::write(
        &with_unknown,
        r#"{ "noSuchKey": 123, "worker": { "alsoUnknown": true, "model": "opus" } }"#,
    )
    .unwrap();

    let cfg = config::load_layers(&[missing, with_unknown]).unwrap();
    assert_eq!(cfg.worker.model, "opus");
}

#[test]
fn load_points_at_project_config_under_repo_root() {
    let repo = tempfile::tempdir().unwrap();
    let kranz_dir = repo.path().join(".kranz");
    std::fs::create_dir_all(&kranz_dir).unwrap();
    // Distinctive value: the project layer is last, so nothing (not even a
    // real ~/.kranz/config.json on this machine) can override it.
    std::fs::write(
        kranz_dir.join("config.json"),
        r#"{ "worker": { "model": "kranz-test-model" } }"#,
    )
    .unwrap();

    let cfg = config::load(repo.path()).unwrap();
    assert_eq!(cfg.worker.model, "kranz-test-model");
}

#[test]
fn unparseable_config_file_is_a_config_error_naming_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let bad = dir.path().join("bad-config.json");
    std::fs::write(&bad, "{ not json").unwrap();

    let err = config::load_layers(std::slice::from_ref(&bad)).unwrap_err();
    match err {
        EngineError::Config(msg) => {
            assert!(
                msg.contains("bad-config.json"),
                "error must name the offending file, got: {msg}"
            );
        }
        other => panic!("expected EngineError::Config, got: {other:?}"),
    }
}

#[test]
fn invalid_reasoning_effort_rejected() {
    let cfg = cfg_with(|c| c.worker.reasoning_effort = "ultra".into());
    let err = config::validate(&cfg).unwrap_err();
    assert!(matches!(err, EngineError::Config(_)), "got: {err:?}");
}

#[test]
fn max_parallel_workers_bounds() {
    // roadmap M3: 1 (default, sequential) through 8 validate; >1 opts into
    // parallel workers. 0 (no worker can ever run) and 9 (past useful fan-out)
    // are rejected.
    for n in 1..=8 {
        let cfg = cfg_with(|c| c.max_parallel_workers = n);
        assert!(
            config::validate(&cfg).is_ok(),
            "maxParallelWorkers={n} must validate"
        );
    }
    for bad in [0, 9] {
        let cfg = cfg_with(|c| c.max_parallel_workers = bad);
        let err = config::validate(&cfg).unwrap_err();
        assert!(
            matches!(err, EngineError::Config(_)),
            "maxParallelWorkers={bad} must be rejected, got: {err:?}"
        );
    }
}

#[test]
fn other_validate_bounds() {
    let cfg = cfg_with(|c| c.max_fix_cycles_per_milestone = 0);
    assert!(config::validate(&cfg).is_err());

    let mut cfg = cfg_with(|c| c.max_respawns = 6);
    assert!(config::validate(&cfg).is_err());
    cfg.max_respawns = 5;
    assert!(config::validate(&cfg).is_ok());

    let mut cfg = cfg_with(|c| c.event_stream_throttle_ms = 9);
    assert!(config::validate(&cfg).is_err());
    cfg.event_stream_throttle_ms = 5001;
    assert!(config::validate(&cfg).is_err());
    cfg.event_stream_throttle_ms = 10;
    assert!(config::validate(&cfg).is_ok());
}

#[test]
fn deep_merge_nested_objects_merge_and_scalars_replace() {
    let mut base = serde_json::json!({
        "a": { "b": 1, "c": 2, "deep": { "x": true } },
        "d": 3,
        "arr": [1, 2, 3]
    });
    let patch = serde_json::json!({
        "a": { "b": 9, "deep": { "y": false } },
        "e": 4,
        "arr": [7]
    });
    config::deep_merge(&mut base, &patch);
    assert_eq!(
        base,
        serde_json::json!({
            // objects merge key-wise, recursively
            "a": { "b": 9, "c": 2, "deep": { "x": true, "y": false } },
            "d": 3,
            // arrays are replaced wholesale, not merged
            "arr": [7],
            "e": 4
        })
    );

    // Non-object patch replaces the base value entirely.
    let mut base = serde_json::json!({ "a": 1 });
    config::deep_merge(&mut base, &serde_json::json!(42));
    assert_eq!(base, serde_json::json!(42));
}

// ---------------------------------------------------------------------------
// cost
// ---------------------------------------------------------------------------

#[test]
fn pricing_table_matches_plan() {
    let cases = [
        ("claude-fable-5", 10.0, 50.0),
        ("OPUS", 5.0, 25.0),
        ("claude-sonnet-4-5", 3.0, 15.0),
        ("Claude-Haiku-3", 1.0, 5.0),
        ("some-unknown-model", 5.0, 25.0),
    ];
    for (model, input, output) in cases {
        let p = cost::pricing_for_model(model);
        approx(p.input_per_mtok, input);
        approx(p.output_per_mtok, output);
        // Cache rates derive from the input rate.
        approx(p.cache_read_per_mtok(), 0.1 * input);
        approx(p.cache_write_per_mtok(), 1.25 * input);
    }
}

#[test]
fn usage_cost_hand_computed() {
    let usage = TokenUsage {
        input: 1_000_000,
        output: 500_000,
        cache_read: 2_000_000,
        cache_write: 400_000,
    };
    // sonnet: 1.0*3 + 0.5*15 + 2.0*(0.1*3) + 0.4*(1.25*3)
    //       = 3.0  + 7.5    + 0.6          + 1.5           = 12.6
    approx(cost::usage_cost_usd(&usage, "claude-sonnet-4"), 12.6);
}

fn plan_with(features_per_milestone: &[usize]) -> Plan {
    Plan {
        goal: "test goal".into(),
        validation_contract: vec![],
        milestones: features_per_milestone
            .iter()
            .enumerate()
            .map(|(i, &n)| PlanMilestone {
                title: format!("m{i}"),
                features: (0..n)
                    .map(|j| PlanFeature {
                        title: format!("f{i}-{j}"),
                        spec: "spec".into(),
                        validation_criteria: vec!["it works".into()],
                    })
                    .collect(),
            })
            .collect(),
        considered_alternatives: None,
        command_grants: vec![],
        touch_set: vec![],
    }
}

#[test]
fn estimate_matches_hand_computed_formula() {
    // 2 milestones, 5 features total, default params:
    //   r=0.2, x=0.5, f=2.0, worker=$1.50, validator=$0.75, orch=$0.25
    // worker_runs    = 5*1.2 + 2*0.5*2.0*1.2 = 6.0 + 2.4 = 8.4
    // validator_runs = 2*2*(1+0.5)           = 6.0
    // expected       = 8.4*1.5 + 6.0*0.75 + 5*0.25 = 12.6 + 4.5 + 1.25 = 18.35
    let plan = plan_with(&[3, 2]);
    let cfg = MissionConfig::default();
    let est = cost::estimate(&plan, &cfg, &EstimateParams::default());

    approx(est.worker_runs, 8.4);
    approx(est.validator_runs, 6.0);
    approx(est.expected_usd, 18.35);
    approx(est.low_usd, 0.5 * 18.35);
    approx(est.high_usd, 2.5 * 18.35);
}

#[test]
fn estimate_respects_skip_scrutiny() {
    let plan = plan_with(&[3, 2]);
    let mut cfg = cfg_with(|c| c.skip_scrutiny = true);
    let est = cost::estimate(&plan, &cfg, &EstimateParams::default());

    // Half the validator pairs gone: validator_runs = 1*2*1.5 = 3.0
    approx(est.validator_runs, 3.0);
    approx(est.worker_runs, 8.4); // unchanged
    approx(est.expected_usd, 12.6 + 3.0 * 0.75 + 1.25); // 16.1

    // Skipping both removes all validator runs.
    cfg.skip_functional = true;
    let est = cost::estimate(&plan, &cfg, &EstimateParams::default());
    approx(est.validator_runs, 0.0);
    approx(est.expected_usd, 12.6 + 1.25);
}

// ---------------------------------------------------------------------------
// cost: calibration from recorded actuals
// ---------------------------------------------------------------------------

/// Write a hand-built events.jsonl for `mission_id` under `repo` (seq
/// assigned 1..; same shape the engine's single writer produces).
fn write_events(repo: &Path, mission_id: &str, kinds: Vec<EventKind>) {
    let dir = repo.join(".kranz").join("missions").join(mission_id);
    std::fs::create_dir_all(&dir).unwrap();
    let mut lines = String::new();
    for (i, kind) in kinds.into_iter().enumerate() {
        let event = Event {
            seq: (i + 1) as u64,
            ts: Utc::now(),
            mission_id: mission_id.to_string(),
            kind,
        };
        lines.push_str(&serde_json::to_string(&event).unwrap());
        lines.push('\n');
    }
    std::fs::write(dir.join("events.jsonl"), lines).unwrap();
}

fn created(mission_id: &str) -> EventKind {
    EventKind::MissionCreated {
        goal: "calibration fixture".to_string(),
        base_branch: "main".to_string(),
        mission_branch: format!("kranz/mission-{mission_id}"),
        config: MissionConfig::default(),
    }
}

fn spawned(
    run_id: &str,
    role: Role,
    feature_id: Option<&str>,
    milestone_id: Option<&str>,
) -> EventKind {
    EventKind::WorkerSpawned {
        run_id: run_id.to_string(),
        role,
        feature_id: feature_id.map(str::to_string),
        milestone_id: milestone_id.map(str::to_string),
        candidate: None,
        executor_route: None,
        sdk_session_id: format!("sess-{run_id}"),
        model: "sonnet".to_string(),
        quant: "n/a".to_string(),
        weight_hash: None,
        prompt_hash: "hash".to_string(),
        transcript_path: format!("runs/{run_id}.jsonl"),
    }
}

fn completed(run_id: &str, cost_usd: Option<f64>, tokens: TokenUsage) -> EventKind {
    EventKind::WorkerCompleted {
        run_id: run_id.to_string(),
        result: RunResult::Pass,
        tokens,
        cost_usd,
        report: None,
    }
}

fn fix_feature(id: &str) -> Feature {
    Feature {
        id: id.to_string(),
        title: "fix it".to_string(),
        spec: "address the finding".to_string(),
        validation_criteria: vec![],
        origin: FeatureOrigin::Fix,
        status: FeatureStatus::Pending,
        worker_runs: vec![],
        commits: vec![],
        respawns: 0,
    }
}

/// Mission A: 2 planned features + 1 fix feature over 1 milestone.
/// Worker costs 1.0, 2.0, 3.0 (usage fallback), 2.0 → avg 2.0; validator
/// costs 0.6, 1.0 → avg 0.8; 1 respawn / 2 planned → r 0.5; 1 fix cycle /
/// 1 milestone → x 1.0; 1 fix feature / 1 cycle → f 1.0; orchestrator 0.9 /
/// 3 features → 0.3.
fn mission_a_events() -> Vec<EventKind> {
    vec![
        created("m-a"),
        EventKind::PlanApproved {
            plan: plan_with(&[2]),
            base_sha: None,
        },
        EventKind::MilestoneStarted {
            milestone_id: "ms-1".into(),
            start_sha: "aaa".into(),
        },
        EventKind::FeatureStarted {
            feature_id: "f-1-1".into(),
        },
        spawned("w-1", Role::Worker, Some("f-1-1"), None),
        completed("w-1", Some(1.0), TokenUsage::default()),
        EventKind::FeatureCompleted {
            feature_id: "f-1-1".into(),
            commits: vec![],
        },
        EventKind::FeatureStarted {
            feature_id: "f-1-2".into(),
        },
        spawned("w-2", Role::Worker, Some("f-1-2"), None),
        completed("w-2", Some(2.0), TokenUsage::default()),
        // Respawn on f-1-2 (2nd run on the same feature)...
        spawned("w-3", Role::Worker, Some("f-1-2"), None),
        // ...whose cost is unreported: falls back to usage pricing
        // (1 MTok sonnet input = $3.00).
        completed(
            "w-3",
            None,
            TokenUsage {
                input: 1_000_000,
                output: 0,
                cache_read: 0,
                cache_write: 0,
            },
        ),
        EventKind::FeatureCompleted {
            feature_id: "f-1-2".into(),
            commits: vec![],
        },
        EventKind::MilestoneValidating {
            milestone_id: "ms-1".into(),
        },
        spawned("v-1", Role::ValidatorScrutiny, None, Some("ms-1")),
        completed("v-1", Some(0.6), TokenUsage::default()),
        EventKind::ValidationFinding {
            milestone_id: "ms-1".into(),
            run_id: "v-1".into(),
            finding: Finding {
                subject: "a-1".into(),
                severity: "major".into(),
                evidence: "it broke".into(),
                suggested_fix: "fix it".into(),
                class: String::new(),
            },
        },
        // First fix-feature after milestone.validating: fix cycle #1.
        EventKind::FixFeatureCreated {
            milestone_id: "ms-1".into(),
            feature: fix_feature("f-fix-1"),
        },
        EventKind::FeatureStarted {
            feature_id: "f-fix-1".into(),
        },
        spawned("w-4", Role::Worker, Some("f-fix-1"), None),
        completed("w-4", Some(2.0), TokenUsage::default()),
        EventKind::FeatureCompleted {
            feature_id: "f-fix-1".into(),
            commits: vec![],
        },
        EventKind::MilestoneValidating {
            milestone_id: "ms-1".into(),
        },
        spawned("v-2", Role::ValidatorFunctional, None, Some("ms-1")),
        completed("v-2", Some(1.0), TokenUsage::default()),
        EventKind::MilestoneCompleted {
            milestone_id: "ms-1".into(),
            tag: None,
        },
        spawned("o-1", Role::Orchestrator, None, None),
        completed("o-1", Some(0.9), TokenUsage::default()),
        EventKind::MissionCompleted {},
    ]
}

/// Mission B: the trivial clean run. Worker avg 1.0; validator avg 0.4; no
/// respawns, no fix cycles, no fix features; orchestrator 0.5 / 1 feature.
fn mission_b_events() -> Vec<EventKind> {
    vec![
        created("m-b"),
        EventKind::PlanApproved {
            plan: plan_with(&[1]),
            base_sha: None,
        },
        EventKind::MilestoneStarted {
            milestone_id: "ms-1".into(),
            start_sha: "bbb".into(),
        },
        EventKind::FeatureStarted {
            feature_id: "f-1-1".into(),
        },
        spawned("w-1", Role::Worker, Some("f-1-1"), None),
        completed("w-1", Some(1.0), TokenUsage::default()),
        EventKind::FeatureCompleted {
            feature_id: "f-1-1".into(),
            commits: vec![],
        },
        EventKind::MilestoneValidating {
            milestone_id: "ms-1".into(),
        },
        spawned("v-1", Role::ValidatorScrutiny, None, Some("ms-1")),
        completed("v-1", Some(0.4), TokenUsage::default()),
        EventKind::MilestoneCompleted {
            milestone_id: "ms-1".into(),
            tag: None,
        },
        spawned("o-1", Role::Orchestrator, None, None),
        completed("o-1", Some(0.5), TokenUsage::default()),
        EventKind::MissionCompleted {},
    ]
}

#[test]
fn calibrate_averages_actuals_across_completed_missions() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();

    write_events(repo, "m-a", mission_a_events());
    write_events(repo, "m-b", mission_b_events());
    // Distractors, none of which may count or fail the calibration:
    // an in-flight mission (folds fine, but is not Complete)...
    write_events(
        repo,
        "m-running",
        vec![
            created("m-running"),
            EventKind::PlanApproved {
                plan: plan_with(&[1]),
                base_sha: None,
            },
        ],
    );
    // ...an unreadable log (corruption mid-file)...
    let bad = repo.join(".kranz").join("missions").join("m-bad");
    std::fs::create_dir_all(&bad).unwrap();
    std::fs::write(bad.join("events.jsonl"), "not json\nalso not json\n").unwrap();
    // ...and a mission directory with no log at all.
    std::fs::create_dir_all(repo.join(".kranz").join("missions").join("m-empty")).unwrap();

    let calibration = cost::calibrate(repo);

    assert_eq!(calibration.missions_used, 2);
    let p = calibration.params;
    // Simple means of the two missions' hand-computed actuals (see the
    // fixture doc comments): A=(2.0, 0.8, 0.5, 1.0, 1.0, 0.3), B=(1.0, 0.4,
    // 0.0, 0.0, 0.0, 0.5).
    approx(p.avg_worker_run_usd, 1.5);
    approx(p.avg_validator_run_usd, 0.6);
    approx(p.respawn_allowance, 0.25);
    approx(p.fix_cycles_per_milestone, 0.5);
    approx(p.fix_features_per_cycle, 0.5);
    approx(p.orchestrator_overhead_usd_per_feature, 0.4);
}

#[test]
fn calibrate_without_completed_missions_returns_defaults() {
    // A repo with no missions at all.
    let tmp = tempfile::tempdir().unwrap();
    let calibration = cost::calibrate(tmp.path());
    assert_eq!(calibration.missions_used, 0);
    assert_eq!(calibration.params, EstimateParams::default());

    // A repo whose only mission log is unreadable: skipped → defaults again.
    let tmp = tempfile::tempdir().unwrap();
    let bad = tmp.path().join(".kranz").join("missions").join("m-bad");
    std::fs::create_dir_all(&bad).unwrap();
    std::fs::write(bad.join("events.jsonl"), "garbage\nmore garbage\n").unwrap();
    let calibration = cost::calibrate(tmp.path());
    assert_eq!(calibration.missions_used, 0);
    assert_eq!(calibration.params, EstimateParams::default());
}

#[test]
fn calibrate_clamps_zero_costs_to_floor() {
    // A completed mission that reported $0 for everything (and had no
    // validator or orchestrator runs) must not zero future estimates.
    let tmp = tempfile::tempdir().unwrap();
    write_events(
        tmp.path(),
        "m-zero",
        vec![
            created("m-zero"),
            EventKind::PlanApproved {
                plan: plan_with(&[1]),
                base_sha: None,
            },
            EventKind::MilestoneStarted {
                milestone_id: "ms-1".into(),
                start_sha: "ccc".into(),
            },
            EventKind::FeatureStarted {
                feature_id: "f-1-1".into(),
            },
            spawned("w-1", Role::Worker, Some("f-1-1"), None),
            completed("w-1", Some(0.0), TokenUsage::default()),
            EventKind::FeatureCompleted {
                feature_id: "f-1-1".into(),
                commits: vec![],
            },
            EventKind::MilestoneCompleted {
                milestone_id: "ms-1".into(),
                tag: None,
            },
            EventKind::MissionCompleted {},
        ],
    );

    let calibration = cost::calibrate(tmp.path());
    assert_eq!(calibration.missions_used, 1);
    let p = calibration.params;
    approx(p.avg_worker_run_usd, 0.01);
    approx(p.avg_validator_run_usd, 0.01);
    approx(p.orchestrator_overhead_usd_per_feature, 0.01);
    approx(p.respawn_allowance, 0.0);
    approx(p.fix_cycles_per_milestone, 0.0);
    approx(p.fix_features_per_cycle, 0.0);
}

fn completed_mission_with_worker_cost(id: &str, worker_cost: f64) -> Vec<EventKind> {
    vec![
        created(id),
        EventKind::PlanApproved {
            plan: plan_with(&[1]),
            base_sha: None,
        },
        EventKind::MilestoneStarted {
            milestone_id: "ms-1".into(),
            start_sha: "s".into(),
        },
        EventKind::FeatureStarted {
            feature_id: "f-1-1".into(),
        },
        spawned("w-1", Role::Worker, Some("f-1-1"), None),
        completed("w-1", Some(worker_cost), TokenUsage::default()),
        EventKind::FeatureCompleted {
            feature_id: "f-1-1".into(),
            commits: vec![],
        },
        EventKind::MilestoneCompleted {
            milestone_id: "ms-1".into(),
            tag: None,
        },
        EventKind::MissionCompleted {},
    ]
}

#[test]
fn corpus_fit_is_off_below_the_minimum_mission_count() {
    // Below MIN_CALIBRATION_MISSIONS the corpus can't be fit, so the built-in
    // 1.0 / 0.5 / 2.5 band is kept and the estimate is unchanged from before
    // the M1 refit.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    for i in 0..(cost::MIN_CALIBRATION_MISSIONS - 1) {
        write_events(
            repo,
            &format!("m-{i}"),
            completed_mission_with_worker_cost(&format!("m-{i}"), 5.0),
        );
    }
    let cal = cost::calibrate(repo);
    assert_eq!(cal.missions_used, cost::MIN_CALIBRATION_MISSIONS - 1);
    approx(cal.expected_mult, 1.0);
    approx(cal.low_mult, 0.5);
    approx(cal.high_mult, 2.5);
}

#[test]
fn corpus_fit_engages_and_drives_apply_shape() {
    // At/above the threshold the fit runs: the range becomes the empirical
    // p10/p90 of actual÷predicted (not the fixed 0.5/2.5), and apply_shape
    // multiplies the raw estimate by the fitted mults.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    // Heterogeneous actuals so the fitted range is non-degenerate.
    for (i, cost_usd) in [2.0, 4.0, 6.0, 8.0, 10.0, 30.0].iter().enumerate() {
        write_events(
            repo,
            &format!("m-{i}"),
            completed_mission_with_worker_cost(&format!("m-{i}"), *cost_usd),
        );
    }
    let cal = cost::calibrate(repo);
    assert_eq!(cal.missions_used, 6);

    // The fit engaged: the band is no longer the built-in guess, and it is
    // ordered around the center.
    assert!(
        (cal.low_mult, cal.high_mult) != (0.5, 2.5),
        "expected an empirically fitted band, got ({}, {})",
        cal.low_mult,
        cal.high_mult
    );
    assert!(
        cal.low_mult <= cal.expected_mult && cal.expected_mult <= cal.high_mult,
        "band must bracket the center: {} <= {} <= {}",
        cal.low_mult,
        cal.expected_mult,
        cal.high_mult
    );

    // apply_shape applies the fit to a code-shape plan (no doc-heavy widening).
    let mut plan = plan_with(&[2, 1]);
    plan.validation_contract = vec![command_assertion("a1", "cargo test --workspace")];
    let cfg = MissionConfig::default();
    let base = cost::estimate(&plan, &cfg, &cal.params);
    let est = cost::apply_shape(base, &plan, &cal);
    assert_eq!(est.shape, cost::MissionShape::CodeChange);
    approx(est.expected_usd, base.expected_usd * cal.expected_mult);
    approx(est.low_usd, base.expected_usd * cal.low_mult);
    approx(est.high_usd, base.expected_usd * cal.high_mult);
}

// ---------------------------------------------------------------------------
// prompts
// ---------------------------------------------------------------------------

const ALL_ROLES: [Role; 4] = [
    Role::Orchestrator,
    Role::Worker,
    Role::ValidatorScrutiny,
    Role::ValidatorFunctional,
];

#[test]
fn prompt_files_non_empty_with_required_placeholders() {
    for role in ALL_ROLES {
        assert!(
            !prompts::text(role).trim().is_empty(),
            "prompt for {role:?} is empty"
        );
    }
    assert!(prompts::text(Role::Orchestrator).contains("{turnBudget}"));
    assert!(prompts::text(Role::Worker).contains("{turnBudget}"));
    assert!(prompts::text(Role::Worker).contains("{featureId}"));
    assert!(prompts::text(Role::ValidatorScrutiny).contains("{startSha}"));
}

#[test]
fn worker_prompt_contains_worker_report_field_names() {
    let worker = prompts::text(Role::Worker);
    for field in [
        "result",
        "summary",
        "filesTouched",
        "testsAdded",
        "testEvidence",
        "dependenciesAdded",
        "knownGaps",
        "commits",
    ] {
        assert!(
            worker.contains(field),
            "worker prompt missing WorkerReport field {field:?}"
        );
    }
}

#[test]
fn prompts_state_contract_lives_in_approved_plan_json() {
    for (role, name) in [
        (Role::Worker, "Worker"),
        (Role::Orchestrator, "Orchestrator"),
    ] {
        let text = prompts::text(role).to_lowercase();
        for needle in ["plan.json", "plan.md", "operator", "escalat"] {
            assert!(
                text.contains(needle),
                "{name} prompt missing required substring {needle:?}"
            );
        }
    }
}

#[test]
fn prompt_hashes_are_12_hex_chars_and_distinct() {
    let hashes: Vec<String> = ALL_ROLES.iter().map(|&r| prompts::hash(r)).collect();
    for h in &hashes {
        assert_eq!(h.len(), 12, "hash {h:?} is not 12 chars");
        assert!(
            h.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "hash {h:?} is not lowercase hex"
        );
    }
    let unique: std::collections::HashSet<&String> = hashes.iter().collect();
    assert_eq!(
        unique.len(),
        ALL_ROLES.len(),
        "role hashes must differ: {hashes:?}"
    );
}

#[test]
fn render_replaces_all_occurrences_and_keeps_unknown_placeholders() {
    let mut vars: HashMap<&str, String> = HashMap::new();
    vars.insert("featureId", "f-7".into());
    vars.insert("turnBudget", "50".into());

    let out = prompts::render(
        "Feature {featureId}: budget {turnBudget} turns. Commit as [{featureId}]. Unknown {nope} stays. JSON braces {\"ok\": true} stay.",
        &vars,
    );
    assert_eq!(
        out,
        "Feature f-7: budget 50 turns. Commit as [f-7]. Unknown {nope} stays. JSON braces {\"ok\": true} stay."
    );
}

#[test]
fn render_does_not_recurse_into_replacement_values() {
    let mut vars: HashMap<&str, String> = HashMap::new();
    vars.insert("a", "{b}".into());
    vars.insert("b", "X".into());
    // "{a}" becomes literal "{b}" and must NOT then expand to "X".
    assert_eq!(prompts::render("{a} {b}", &vars), "{b} X");
}

// The test file name promises a PathBuf-based layering API; keep the type
// contract explicit so refactors don't silently change it.
#[test]
fn load_layers_takes_pathbuf_slices() {
    let layers: Vec<PathBuf> = vec![];
    let cfg = config::load_layers(&layers).unwrap();
    assert_eq!(cfg, MissionConfig::default());
}

// ---------------------------------------------------------------------------
// cost::classify_shape
// ---------------------------------------------------------------------------

fn judgement_assertion(id: &str) -> Assertion {
    Assertion {
        id: id.into(),
        statement: "orchestrator judges the full diff".into(),
        check: AssertionCheck::AgentJudgement,
        command: None,
    }
}

fn command_assertion(id: &str, command: &str) -> Assertion {
    Assertion {
        id: id.into(),
        statement: "a command gate".into(),
        check: AssertionCheck::Command,
        command: Some(command.into()),
    }
}

fn plan_with_contract(validation_contract: Vec<Assertion>) -> Plan {
    let mut plan = plan_with(&[1]);
    plan.validation_contract = validation_contract;
    plan
}

#[test]
fn classify_shape_judgement_heavy_grep_only_contract_is_doc_heavy() {
    // Mirrors m-d341a7's contract shape: judgement assertions plus grep/git
    // command assertions, but nothing that gates on a build or test.
    let plan = plan_with_contract(vec![
        judgement_assertion("a1"),
        judgement_assertion("a2"),
        command_assertion("a3", "bash -c 'grep -q foo doc.md'"),
        command_assertion("a4", "bash -c 'git diff --stat'"),
        command_assertion("a5", "bash -c 'grep -c bar doc.md'"),
    ]);
    assert_eq!(cost::classify_shape(&plan), cost::MissionShape::DocHeavy);
}

#[test]
fn classify_shape_empty_contract_is_unknown() {
    let plan = plan_with_contract(vec![]);
    assert_eq!(cost::classify_shape(&plan), cost::MissionShape::Unknown);
}

#[test]
fn classify_shape_cargo_test_command_wins_even_with_judgement() {
    let plan = plan_with_contract(vec![
        judgement_assertion("a1"),
        command_assertion("a2", "cargo test --workspace"),
    ]);
    assert_eq!(cost::classify_shape(&plan), cost::MissionShape::CodeChange);
}

#[test]
fn classify_shape_grep_only_no_judgement_is_unknown() {
    let plan = plan_with_contract(vec![
        command_assertion("a1", "bash -c 'grep -q foo doc.md'"),
        command_assertion("a2", "bash -c 'grep -c bar doc.md'"),
    ]);
    assert_eq!(cost::classify_shape(&plan), cost::MissionShape::Unknown);
}

// ---------------------------------------------------------------------------
// cost::apply_shape (confidence-gated shape widening)
// ---------------------------------------------------------------------------

/// m-d341a7's recorded actual cost, per its report.md / event log — the
/// judgement-heavy mission that motivated shape-aware widening: its
/// contract has 2 agent-judgement assertions and no build/test command, so
/// it classifies as DocHeavy, yet ran ~9x over an $18.35 base estimate.
const M_D341A7_ACTUAL_USD: f64 = 163.64;

fn code_only_calibration(repo: &Path) -> cost::Calibration {
    write_events(repo, "m-a", mission_a_events());
    write_events(repo, "m-b", mission_b_events());
    cost::calibrate(repo)
}

#[test]
fn backtest_m_d341a7_doc_heavy_lands_in_range() {
    let plan: Plan = serde_json::from_str(include_str!("fixtures/m-d341a7-plan.json")).unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let calibration = code_only_calibration(tmp.path());
    assert_eq!(calibration.doc_heavy_missions_used, 0);

    let cfg = MissionConfig::default();
    let base = cost::estimate(&plan, &cfg, &calibration.params);
    let est = cost::apply_shape(base, &plan, &calibration);

    assert_eq!(est.shape, cost::MissionShape::DocHeavy);
    assert_eq!(est.confidence, cost::Confidence::Low);
    assert!(
        est.low_usd <= M_D341A7_ACTUAL_USD && M_D341A7_ACTUAL_USD <= est.high_usd,
        "range [{}, {}] must bracket the recorded actual {}",
        est.low_usd,
        est.high_usd,
        M_D341A7_ACTUAL_USD
    );
}

#[test]
fn estimate_code_shape_unchanged_by_apply_shape() {
    let mut plan = plan_with(&[3, 2]);
    plan.validation_contract = vec![command_assertion("a1", "cargo test --workspace")];
    let cfg = MissionConfig::default();

    let tmp = tempfile::tempdir().unwrap();
    let calibration = code_only_calibration(tmp.path());

    let base = cost::estimate(&plan, &cfg, &calibration.params);
    let est = cost::apply_shape(base, &plan, &calibration);

    assert_eq!(est.shape, cost::MissionShape::CodeChange);
    assert_eq!(est.confidence, cost::Confidence::High);
    approx(est.low_usd, base.low_usd);
    approx(est.expected_usd, base.expected_usd);
    approx(est.high_usd, base.high_usd);
}

#[test]
fn apply_shape_noop_for_unknown_contract() {
    let plan = plan_with_contract(vec![]);
    let cfg = MissionConfig::default();

    let tmp = tempfile::tempdir().unwrap();
    let calibration = code_only_calibration(tmp.path());

    let base = cost::estimate(&plan, &cfg, &calibration.params);
    let est = cost::apply_shape(base, &plan, &calibration);

    assert_eq!(est.shape, cost::MissionShape::Unknown);
    assert_eq!(est.confidence, cost::Confidence::High);
    approx(est.low_usd, base.low_usd);
    approx(est.expected_usd, base.expected_usd);
    approx(est.high_usd, base.high_usd);
}

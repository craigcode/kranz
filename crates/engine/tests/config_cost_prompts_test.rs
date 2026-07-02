//! Integration tests for config layering, cost estimation, and role prompts.

use kranz_engine::config;
use kranz_engine::cost::{self, EstimateParams};
use kranz_engine::error::EngineError;
use kranz_engine::prompts;
use kranz_engine::types::{
    MissionConfig, Plan, PlanFeature, PlanMilestone, Role, TokenUsage,
};
use std::collections::HashMap;
use std::path::PathBuf;

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
    assert_eq!(cfg.max_fix_cycles_per_milestone, default.max_fix_cycles_per_milestone);
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
fn parallel_workers_must_be_one_in_v1() {
    let cfg = cfg_with(|c| c.max_parallel_workers = 2);
    let err = config::validate(&cfg).unwrap_err();
    assert!(matches!(err, EngineError::Config(_)), "got: {err:?}");
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
fn prompt_hashes_are_12_hex_chars_and_distinct() {
    let hashes: Vec<String> = ALL_ROLES.iter().map(|&r| prompts::hash(r)).collect();
    for h in &hashes {
        assert_eq!(h.len(), 12, "hash {h:?} is not 12 chars");
        assert!(
            h.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "hash {h:?} is not lowercase hex"
        );
    }
    let unique: std::collections::HashSet<&String> = hashes.iter().collect();
    assert_eq!(unique.len(), ALL_ROLES.len(), "role hashes must differ: {hashes:?}");
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

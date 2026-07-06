//! Integration tests for the pure event fold (plan §4.3/§4.4 semantics).

use chrono::{DateTime, TimeZone, Utc};
use kranz_engine::error::EngineError;
use kranz_engine::events::{Event, EventKind};
use kranz_engine::reducer::{apply, fold, read_snapshot, write_snapshot};
use kranz_engine::types::*;
use proptest::prelude::*;
use serde_json::json;

const MISSION: &str = "m-1";

fn base_ts() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap()
}

/// Deterministic event constructor: ts = base + seq seconds.
fn ev(seq: u64, kind: EventKind) -> Event {
    Event {
        seq,
        ts: base_ts() + chrono::Duration::seconds(seq as i64),
        mission_id: MISSION.to_string(),
        kind,
    }
}

fn created() -> EventKind {
    EventKind::MissionCreated {
        goal: "build the thing".to_string(),
        base_branch: "main".to_string(),
        mission_branch: format!("kranz/mission-{MISSION}"),
        config: MissionConfig::default(),
    }
}

/// Plan with 2 milestones / 3 features: ms-1 { f-1-1, f-1-2 }, ms-2 { f-2-1 }.
fn plan() -> Plan {
    let feature = |title: &str| PlanFeature {
        title: title.to_string(),
        spec: format!("spec for {title}"),
        validation_criteria: vec![format!("{title} works")],
    };
    Plan {
        goal: "build the thing, planned".to_string(),
        validation_contract: vec![Assertion {
            id: "a-1".to_string(),
            statement: "cargo test passes".to_string(),
            check: AssertionCheck::Command,
            command: Some("cargo test".to_string()),
        }],
        milestones: vec![
            PlanMilestone {
                title: "milestone one".to_string(),
                features: vec![feature("alpha"), feature("beta")],
            },
            PlanMilestone {
                title: "milestone two".to_string(),
                features: vec![feature("gamma")],
            },
        ],
        command_grants: vec![],
        touch_set: vec![],
    }
}

fn spawn(run_id: &str, feature_id: Option<&str>, milestone_id: Option<&str>) -> EventKind {
    EventKind::WorkerSpawned {
        run_id: run_id.to_string(),
        role: if feature_id.is_some() {
            Role::Worker
        } else {
            Role::ValidatorScrutiny
        },
        feature_id: feature_id.map(str::to_string),
        milestone_id: milestone_id.map(str::to_string),
        sdk_session_id: format!("sess-{run_id}"),
        model: "sonnet".to_string(),
        prompt_hash: "deadbeef".to_string(),
        transcript_path: format!("runs/{run_id}.jsonl"),
    }
}

fn completed(run_id: &str, tokens: TokenUsage, cost: Option<f64>) -> EventKind {
    EventKind::WorkerCompleted {
        run_id: run_id.to_string(),
        result: RunResult::Pass,
        tokens,
        cost_usd: cost,
        report: None,
    }
}

fn tokens(input: u64, output: u64) -> TokenUsage {
    TokenUsage {
        input,
        output,
        cache_read: 1,
        cache_write: 2,
    }
}

fn fix_feature(id: &str) -> Feature {
    Feature {
        id: id.to_string(),
        title: format!("fix {id}"),
        spec: "fix it".to_string(),
        validation_criteria: vec!["fixed".to_string()],
        origin: FeatureOrigin::Fix,
        status: FeatureStatus::Pending,
        worker_runs: vec![],
        commits: vec![],
        respawns: 0,
    }
}

fn milestone<'a>(state: &'a MissionState, id: &str) -> &'a Milestone {
    state
        .mission
        .milestones
        .iter()
        .find(|m| m.id == id)
        .unwrap()
}

fn feature<'a>(state: &'a MissionState, id: &str) -> &'a Feature {
    state
        .mission
        .milestones
        .iter()
        .flat_map(|m| m.features.iter())
        .find(|f| f.id == id)
        .unwrap()
}

/// Fold a list of kinds with auto-assigned seqs.
fn fold_kinds(kinds: Vec<EventKind>) -> MissionState {
    let events: Vec<Event> = kinds
        .into_iter()
        .enumerate()
        .map(|(i, k)| ev(i as u64 + 1, k))
        .collect();
    fold(&events).unwrap()
}

// ---------------------------------------------------------------------------
// Golden happy path
// ---------------------------------------------------------------------------

#[test]
fn golden_happy_path() {
    // Stage 1: created.
    let e1 = ev(1, created());
    let mut state = fold(std::slice::from_ref(&e1)).unwrap();
    assert_eq!(state.mission.id, MISSION);
    assert_eq!(state.mission.goal, "build the thing");
    assert_eq!(state.mission.status, MissionStatus::Planning);
    assert_eq!(state.mission.created_at, e1.ts);
    assert_eq!(state.mission.base_branch, "main");
    assert_eq!(state.mission.mission_branch, "kranz/mission-m-1");
    assert!(state.mission.milestones.is_empty());
    assert!(state.mission.validation_contract.is_empty());
    assert_eq!(state.last_seq, 1);
    assert_eq!(state.config, MissionConfig::default());

    // Stage 2: plan approved -> deterministic ids, all pending, Approved.
    apply(
        &mut state,
        &ev(
            2,
            EventKind::PlanApproved {
                plan: plan(),
                base_sha: None,
            },
        ),
    )
    .unwrap();
    assert_eq!(state.mission.status, MissionStatus::Approved);
    assert_eq!(state.mission.goal, "build the thing, planned");
    assert_eq!(state.mission.validation_contract.len(), 1);
    let ms_ids: Vec<&str> = state
        .mission
        .milestones
        .iter()
        .map(|m| m.id.as_str())
        .collect();
    assert_eq!(ms_ids, vec!["ms-1", "ms-2"]);
    let f_ids: Vec<&str> = state
        .mission
        .milestones
        .iter()
        .flat_map(|m| m.features.iter().map(|f| f.id.as_str()))
        .collect();
    assert_eq!(f_ids, vec!["f-1-1", "f-1-2", "f-2-1"]);
    for m in &state.mission.milestones {
        assert_eq!(m.status, MilestoneStatus::Pending);
        assert_eq!(m.fix_cycles, 0);
        assert_eq!(m.start_sha, None);
        for f in &m.features {
            assert_eq!(f.status, FeatureStatus::Pending);
            assert_eq!(f.origin, FeatureOrigin::Plan);
            assert!(f.worker_runs.is_empty());
        }
    }

    // Stage 3: milestone starts.
    apply(
        &mut state,
        &ev(
            3,
            EventKind::MilestoneStarted {
                milestone_id: "ms-1".to_string(),
                start_sha: "sha-1".to_string(),
            },
        ),
    )
    .unwrap();
    assert_eq!(milestone(&state, "ms-1").status, MilestoneStatus::Active);
    assert_eq!(
        milestone(&state, "ms-1").start_sha.as_deref(),
        Some("sha-1")
    );
    assert_eq!(state.mission.status, MissionStatus::Running);

    // Stage 4: feature + worker run.
    apply(
        &mut state,
        &ev(
            4,
            EventKind::FeatureStarted {
                feature_id: "f-1-1".to_string(),
            },
        ),
    )
    .unwrap();
    assert_eq!(feature(&state, "f-1-1").status, FeatureStatus::Active);

    let e5 = ev(5, spawn("r-1", Some("f-1-1"), Some("ms-1")));
    apply(&mut state, &e5).unwrap();
    let run = &state.runs["r-1"];
    assert_eq!(run.role, Role::Worker);
    assert_eq!(run.feature_id.as_deref(), Some("f-1-1"));
    assert_eq!(run.milestone_id.as_deref(), Some("ms-1"));
    assert_eq!(run.started_at, e5.ts);
    assert_eq!(run.ended_at, None);
    assert_eq!(run.tokens, TokenUsage::default());
    assert!(run.result.is_none() && run.report.is_none() && run.cost_usd.is_none());
    assert_eq!(feature(&state, "f-1-1").worker_runs, vec!["r-1"]);
    assert_eq!(feature(&state, "f-1-1").respawns, 0);

    // Stage 5: worker.message changes nothing but last_seq.
    let before = serde_json::to_value(&state).unwrap();
    apply(
        &mut state,
        &ev(
            6,
            EventKind::WorkerMessage {
                run_id: "r-1".to_string(),
                tag: "text".to_string(),
                content: "hi".to_string(),
            },
        ),
    )
    .unwrap();
    let mut after = serde_json::to_value(&state).unwrap();
    after["lastSeq"] = json!(5);
    assert_eq!(before, after);

    // Stage 6: worker completes -> run finalized, totals accumulate.
    let e7 = ev(7, completed("r-1", tokens(100, 50), Some(0.25)));
    apply(&mut state, &e7).unwrap();
    let run = &state.runs["r-1"];
    assert_eq!(run.result, Some(RunResult::Pass));
    assert_eq!(run.tokens, tokens(100, 50));
    assert_eq!(run.cost_usd, Some(0.25));
    assert_eq!(run.ended_at, Some(e7.ts));
    assert_eq!(state.totals, tokens(100, 50));
    assert!((state.total_cost_usd - 0.25).abs() < 1e-12);

    // Stage 7: feature completes with commits.
    apply(
        &mut state,
        &ev(
            8,
            EventKind::FeatureCompleted {
                feature_id: "f-1-1".to_string(),
                commits: vec!["c1".to_string(), "c2".to_string()],
            },
        ),
    )
    .unwrap();
    assert_eq!(feature(&state, "f-1-1").status, FeatureStatus::Complete);
    assert_eq!(feature(&state, "f-1-1").commits, vec!["c1", "c2"]);

    // Stage 8: second feature (with a cost-less run: cost treated as 0).
    apply(
        &mut state,
        &ev(
            9,
            EventKind::FeatureStarted {
                feature_id: "f-1-2".to_string(),
            },
        ),
    )
    .unwrap();
    apply(
        &mut state,
        &ev(10, spawn("r-2", Some("f-1-2"), Some("ms-1"))),
    )
    .unwrap();
    apply(&mut state, &ev(11, completed("r-2", tokens(10, 5), None))).unwrap();
    apply(
        &mut state,
        &ev(
            12,
            EventKind::FeatureCompleted {
                feature_id: "f-1-2".to_string(),
                commits: vec!["c3".to_string()],
            },
        ),
    )
    .unwrap();
    assert_eq!(
        state.totals,
        TokenUsage {
            input: 110,
            output: 55,
            cache_read: 2,
            cache_write: 4
        }
    );
    assert!((state.total_cost_usd - 0.25).abs() < 1e-12);

    // Stage 9: milestone validates clean and completes.
    apply(
        &mut state,
        &ev(
            13,
            EventKind::MilestoneValidating {
                milestone_id: "ms-1".into(),
            },
        ),
    )
    .unwrap();
    assert_eq!(
        milestone(&state, "ms-1").status,
        MilestoneStatus::Validating
    );
    apply(
        &mut state,
        &ev(
            14,
            EventKind::MilestoneCompleted {
                milestone_id: "ms-1".into(),
                tag: None,
            },
        ),
    )
    .unwrap();
    assert_eq!(milestone(&state, "ms-1").status, MilestoneStatus::Complete);
    assert_eq!(milestone(&state, "ms-1").fix_cycles, 0);
    assert_eq!(state.mission.status, MissionStatus::Running);

    // Stage 10: second milestone, then final gate and completion.
    apply(
        &mut state,
        &ev(
            15,
            EventKind::MilestoneStarted {
                milestone_id: "ms-2".to_string(),
                start_sha: "sha-2".to_string(),
            },
        ),
    )
    .unwrap();
    apply(
        &mut state,
        &ev(
            16,
            EventKind::FeatureStarted {
                feature_id: "f-2-1".to_string(),
            },
        ),
    )
    .unwrap();
    apply(
        &mut state,
        &ev(
            17,
            EventKind::FeatureCompleted {
                feature_id: "f-2-1".to_string(),
                commits: vec![],
            },
        ),
    )
    .unwrap();
    apply(
        &mut state,
        &ev(
            18,
            EventKind::MilestoneCompleted {
                milestone_id: "ms-2".into(),
                tag: Some("v1".into()),
            },
        ),
    )
    .unwrap();
    apply(&mut state, &ev(19, EventKind::MissionValidating {})).unwrap();
    assert_eq!(state.mission.status, MissionStatus::Validating);
    apply(&mut state, &ev(20, EventKind::MissionCompleted {})).unwrap();
    assert_eq!(state.mission.status, MissionStatus::Complete);
    assert_eq!(state.last_seq, 20);
}

// ---------------------------------------------------------------------------
// Guards
// ---------------------------------------------------------------------------

#[test]
fn fold_rejects_empty_and_wrong_first_event() {
    assert!(matches!(fold(&[]), Err(EngineError::InvalidState(_))));
    let not_created = ev(1, EventKind::MissionPaused {});
    assert!(matches!(
        fold(&[not_created]),
        Err(EngineError::InvalidState(_))
    ));
}

#[test]
fn apply_rejects_non_contiguous_seq_and_second_created() {
    let mut state = fold(&[ev(1, created())]).unwrap();
    let err = apply(&mut state, &ev(3, EventKind::MissionPaused {})).unwrap_err();
    assert!(matches!(err, EngineError::InvalidState(_)));
    let err = apply(&mut state, &ev(2, created())).unwrap_err();
    assert!(matches!(err, EngineError::InvalidState(_)));
}

#[test]
fn unknown_ids_are_invalid_state() {
    let base = vec![
        ev(1, created()),
        ev(
            2,
            EventKind::PlanApproved {
                plan: plan(),
                base_sha: None,
            },
        ),
    ];
    let cases = vec![
        EventKind::MilestoneStarted {
            milestone_id: "ms-99".into(),
            start_sha: "x".into(),
        },
        EventKind::FeatureStarted {
            feature_id: "f-9-9".into(),
        },
        spawn("r-1", Some("f-9-9"), None),
        spawn("r-1", None, Some("ms-99")),
        EventKind::WorkerMessage {
            run_id: "r-99".into(),
            tag: "text".into(),
            content: "".into(),
        },
        completed("r-99", TokenUsage::default(), None),
        EventKind::MilestoneValidating {
            milestone_id: "ms-99".into(),
        },
        EventKind::FixFeatureCreated {
            milestone_id: "ms-99".into(),
            feature: fix_feature("fx"),
        },
        EventKind::MilestoneBlocked {
            milestone_id: "ms-99".into(),
            reason: "r".into(),
        },
    ];
    for kind in cases {
        let mut state = fold(&base).unwrap();
        let err = apply(&mut state, &ev(3, kind.clone())).unwrap_err();
        assert!(
            matches!(err, EngineError::InvalidState(_)),
            "expected InvalidState for {kind:?}"
        );
    }
}

#[test]
fn respawns_count_second_and_later_runs() {
    let mut state = fold(&[
        ev(1, created()),
        ev(
            2,
            EventKind::PlanApproved {
                plan: plan(),
                base_sha: None,
            },
        ),
    ])
    .unwrap();
    apply(&mut state, &ev(3, spawn("r-1", Some("f-1-1"), None))).unwrap();
    assert_eq!(feature(&state, "f-1-1").respawns, 0);
    apply(&mut state, &ev(4, spawn("r-2", Some("f-1-1"), None))).unwrap();
    assert_eq!(feature(&state, "f-1-1").respawns, 1);
    apply(&mut state, &ev(5, spawn("r-3", Some("f-1-1"), None))).unwrap();
    assert_eq!(feature(&state, "f-1-1").respawns, 2);
    assert_eq!(
        feature(&state, "f-1-1").worker_runs,
        vec!["r-1", "r-2", "r-3"]
    );

    // Duplicate run id is corruption.
    let err = apply(&mut state, &ev(6, spawn("r-1", Some("f-1-2"), None))).unwrap_err();
    assert!(matches!(err, EngineError::InvalidState(_)));
}

// ---------------------------------------------------------------------------
// Fix cycles
// ---------------------------------------------------------------------------

#[test]
fn fix_cycles_increment_once_per_validation_round() {
    let mut state = fold(&[
        ev(1, created()),
        ev(
            2,
            EventKind::PlanApproved {
                plan: plan(),
                base_sha: None,
            },
        ),
        ev(
            3,
            EventKind::MilestoneStarted {
                milestone_id: "ms-1".into(),
                start_sha: "s".into(),
            },
        ),
        ev(
            4,
            EventKind::MilestoneValidating {
                milestone_id: "ms-1".into(),
            },
        ),
        ev(5, spawn("r-v1", None, Some("ms-1"))),
        ev(
            6,
            EventKind::ValidationFinding {
                milestone_id: "ms-1".into(),
                run_id: "r-v1".into(),
                finding: Finding {
                    subject: "a-1".into(),
                    severity: "major".into(),
                    evidence: "it broke".into(),
                    suggested_fix: "fix it".into(),
                    class: String::new(),
                },
            },
        ),
    ])
    .unwrap();
    assert_eq!(
        milestone(&state, "ms-1").status,
        MilestoneStatus::Validating
    );
    assert_eq!(milestone(&state, "ms-1").fix_cycles, 0);

    // Round 1: two fixfeatures, ONE increment.
    apply(
        &mut state,
        &ev(
            7,
            EventKind::FixFeatureCreated {
                milestone_id: "ms-1".into(),
                feature: fix_feature("fx-1"),
            },
        ),
    )
    .unwrap();
    assert_eq!(milestone(&state, "ms-1").fix_cycles, 1);
    assert_eq!(milestone(&state, "ms-1").status, MilestoneStatus::Active);
    apply(
        &mut state,
        &ev(
            8,
            EventKind::FixFeatureCreated {
                milestone_id: "ms-1".into(),
                feature: fix_feature("fx-2"),
            },
        ),
    )
    .unwrap();
    assert_eq!(
        milestone(&state, "ms-1").fix_cycles,
        1,
        "same round must not double-count"
    );
    let fx_ids: Vec<&str> = milestone(&state, "ms-1")
        .features
        .iter()
        .map(|f| f.id.as_str())
        .collect();
    assert_eq!(fx_ids, vec!["f-1-1", "f-1-2", "fx-1", "fx-2"]);
    assert_eq!(feature(&state, "fx-1").origin, FeatureOrigin::Fix);

    // Round 2: validating again, another fixfeature -> second increment.
    apply(
        &mut state,
        &ev(
            9,
            EventKind::MilestoneValidating {
                milestone_id: "ms-1".into(),
            },
        ),
    )
    .unwrap();
    apply(
        &mut state,
        &ev(
            10,
            EventKind::FixFeatureCreated {
                milestone_id: "ms-1".into(),
                feature: fix_feature("fx-3"),
            },
        ),
    )
    .unwrap();
    assert_eq!(milestone(&state, "ms-1").fix_cycles, 2);
}

// ---------------------------------------------------------------------------
// Blocking
// ---------------------------------------------------------------------------

#[test]
fn blocked_and_unblocked_transition_milestone_and_mission() {
    let mut state = fold(&[
        ev(1, created()),
        ev(
            2,
            EventKind::PlanApproved {
                plan: plan(),
                base_sha: None,
            },
        ),
        ev(
            3,
            EventKind::MilestoneStarted {
                milestone_id: "ms-1".into(),
                start_sha: "s".into(),
            },
        ),
    ])
    .unwrap();
    apply(
        &mut state,
        &ev(
            4,
            EventKind::MilestoneBlocked {
                milestone_id: "ms-1".into(),
                reason: "fix cycles exceeded".into(),
            },
        ),
    )
    .unwrap();
    assert_eq!(milestone(&state, "ms-1").status, MilestoneStatus::Blocked);
    assert_eq!(state.mission.status, MissionStatus::Blocked);

    apply(
        &mut state,
        &ev(
            5,
            EventKind::MilestoneUnblocked {
                milestone_id: "ms-1".into(),
                reason: "user raised cap".into(),
            },
        ),
    )
    .unwrap();
    assert_eq!(milestone(&state, "ms-1").status, MilestoneStatus::Active);
    assert_eq!(state.mission.status, MissionStatus::Running);
}

// ---------------------------------------------------------------------------
// No dead states: every status enum value reachable via events
// ---------------------------------------------------------------------------

#[test]
fn every_status_value_is_reachable() {
    let mut mission_seen: Vec<MissionStatus> = Vec::new();
    let mut milestone_seen: Vec<MilestoneStatus> = Vec::new();
    let mut feature_seen: Vec<FeatureStatus> = Vec::new();

    let kinds = vec![
        created(), // Planning
        EventKind::PlanApproved {
            plan: plan(),
            base_sha: None,
        }, // Approved; ms/f Pending
        EventKind::MilestoneStarted {
            milestone_id: "ms-1".into(),
            start_sha: "s".into(),
        }, // Approved -> Running; ms Active
        EventKind::FeatureStarted {
            feature_id: "f-1-1".into(),
        }, // f Active
        EventKind::FeatureCompleted {
            feature_id: "f-1-1".into(),
            commits: vec![],
        }, // f Complete
        EventKind::FeatureFailed {
            feature_id: "f-1-2".into(),
            reason: "r".into(),
        }, // f Failed
        EventKind::FeatureSkipped {
            feature_id: "f-2-1".into(),
            reason: "r".into(),
        }, // f Skipped
        EventKind::MilestoneValidating {
            milestone_id: "ms-1".into(),
        }, // ms Validating
        EventKind::MilestoneBlocked {
            milestone_id: "ms-1".into(),
            reason: "r".into(),
        }, // Blocked x2
        EventKind::MilestoneUnblocked {
            milestone_id: "ms-1".into(),
            reason: "r".into(),
        },
        EventKind::MilestoneCompleted {
            milestone_id: "ms-1".into(),
            tag: None,
        }, // ms Complete
        EventKind::MissionPaused {}, // Paused
        EventKind::MissionResumed {},
        EventKind::MissionValidating {}, // Validating
        EventKind::MissionCompleted {},  // Complete
    ];

    let mut state = fold(&[ev(1, kinds[0].clone())]).unwrap();
    let record = |state: &MissionState,
                  mission_seen: &mut Vec<MissionStatus>,
                  milestone_seen: &mut Vec<MilestoneStatus>,
                  feature_seen: &mut Vec<FeatureStatus>| {
        mission_seen.push(state.mission.status);
        for m in &state.mission.milestones {
            milestone_seen.push(m.status);
            for f in &m.features {
                feature_seen.push(f.status);
            }
        }
    };
    record(
        &state,
        &mut mission_seen,
        &mut milestone_seen,
        &mut feature_seen,
    );
    for (i, kind) in kinds.iter().enumerate().skip(1) {
        apply(&mut state, &ev(i as u64 + 1, kind.clone())).unwrap();
        record(
            &state,
            &mut mission_seen,
            &mut milestone_seen,
            &mut feature_seen,
        );
    }

    // MissionStatus::Failed needs its own history (a mission ends once).
    let failed = fold_kinds(vec![
        created(),
        EventKind::MissionFailed {
            reason: "budget exhausted".into(),
        },
    ]);
    mission_seen.push(failed.mission.status);

    for status in [
        MissionStatus::Planning,
        MissionStatus::Approved,
        MissionStatus::Running,
        MissionStatus::Paused,
        MissionStatus::Blocked,
        MissionStatus::Validating,
        MissionStatus::Complete,
        MissionStatus::Failed,
    ] {
        assert!(
            mission_seen.contains(&status),
            "dead MissionStatus: {status:?}"
        );
    }
    for status in [
        MilestoneStatus::Pending,
        MilestoneStatus::Active,
        MilestoneStatus::Validating,
        MilestoneStatus::Complete,
        MilestoneStatus::Blocked,
    ] {
        assert!(
            milestone_seen.contains(&status),
            "dead MilestoneStatus: {status:?}"
        );
    }
    for status in [
        FeatureStatus::Pending,
        FeatureStatus::Active,
        FeatureStatus::Complete,
        FeatureStatus::Skipped,
        FeatureStatus::Failed,
    ] {
        assert!(
            feature_seen.contains(&status),
            "dead FeatureStatus: {status:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Approved status
// ---------------------------------------------------------------------------

#[test]
fn approved_status_folds_on_plan_approved() {
    let state = fold_kinds(vec![
        created(),
        EventKind::PlanApproved {
            plan: plan(),
            base_sha: None,
        },
    ]);
    assert_eq!(state.mission.status, MissionStatus::Approved);
}

#[test]
fn plan_approved_copies_command_grants_into_mission() {
    let mut granted_plan = plan();
    granted_plan.command_grants = vec!["gc lint".to_string()];
    let state = fold_kinds(vec![
        created(),
        EventKind::PlanApproved {
            plan: granted_plan,
            base_sha: None,
        },
    ]);
    assert_eq!(state.mission.command_grants, vec!["gc lint".to_string()]);
}

#[test]
fn mission_touch_set_round_trips_through_serde() {
    let mut touchy_plan = plan();
    touchy_plan.touch_set = vec!["src/**/*.rs".to_string()];
    let state = fold_kinds(vec![
        created(),
        EventKind::PlanApproved {
            plan: touchy_plan,
            base_sha: None,
        },
    ]);
    let json = serde_json::to_value(&state.mission).unwrap();
    assert_eq!(json["touchSet"], serde_json::json!(["src/**/*.rs"]));
    let round_tripped: Mission = serde_json::from_value(json).unwrap();
    assert_eq!(round_tripped.touch_set, state.mission.touch_set);
}

#[test]
fn plan_approved_copies_touch_set_into_mission() {
    let mut touchy_plan = plan();
    touchy_plan.touch_set = vec!["src/**/*.rs".to_string()];
    let state = fold_kinds(vec![
        created(),
        EventKind::PlanApproved {
            plan: touchy_plan,
            base_sha: None,
        },
    ]);
    assert_eq!(state.mission.touch_set, vec!["src/**/*.rs".to_string()]);
}

#[test]
fn approved_status_transitions_to_running_on_milestone_started() {
    let state = fold_kinds(vec![
        created(),
        EventKind::PlanApproved {
            plan: plan(),
            base_sha: None,
        },
        EventKind::MilestoneStarted {
            milestone_id: "ms-1".into(),
            start_sha: "s".into(),
        },
    ]);
    assert_eq!(state.mission.status, MissionStatus::Running);
}

#[test]
fn approved_status_transitions_to_running_on_worker_spawned() {
    let state = fold_kinds(vec![
        created(),
        EventKind::PlanApproved {
            plan: plan(),
            base_sha: None,
        },
        spawn("r-1", None, None),
    ]);
    assert_eq!(state.mission.status, MissionStatus::Running);
}

#[test]
fn approved_status_full_lifecycle_still_folds_to_complete() {
    let state = fold_kinds(vec![
        created(),
        EventKind::PlanApproved {
            plan: plan(),
            base_sha: None,
        },
        EventKind::MilestoneStarted {
            milestone_id: "ms-1".into(),
            start_sha: "s".into(),
        },
        EventKind::FeatureStarted {
            feature_id: "f-1-1".into(),
        },
        EventKind::FeatureCompleted {
            feature_id: "f-1-1".into(),
            commits: vec![],
        },
        EventKind::FeatureStarted {
            feature_id: "f-1-2".into(),
        },
        EventKind::FeatureCompleted {
            feature_id: "f-1-2".into(),
            commits: vec![],
        },
        EventKind::MilestoneCompleted {
            milestone_id: "ms-1".into(),
            tag: None,
        },
        EventKind::MilestoneStarted {
            milestone_id: "ms-2".into(),
            start_sha: "s2".into(),
        },
        EventKind::FeatureStarted {
            feature_id: "f-2-1".into(),
        },
        EventKind::FeatureCompleted {
            feature_id: "f-2-1".into(),
            commits: vec![],
        },
        EventKind::MilestoneCompleted {
            milestone_id: "ms-2".into(),
            tag: None,
        },
        EventKind::MissionCompleted {},
    ]);
    assert_eq!(state.mission.status, MissionStatus::Complete);
}

#[test]
fn approved_status_guard_never_overwrites_terminal_status() {
    let mut state = fold_kinds(vec![
        created(),
        EventKind::MissionFailed {
            reason: "budget exhausted".into(),
        },
    ]);
    assert_eq!(state.mission.status, MissionStatus::Failed);

    // The guard on MilestoneStarted/WorkerSpawned only fires when the
    // mission is Approved; a terminal status must survive it untouched.
    let next_seq = state.last_seq + 1;
    apply(
        &mut state,
        &ev(
            next_seq,
            EventKind::WorkerSpawned {
                run_id: "r-after-failure".to_string(),
                role: Role::ValidatorScrutiny,
                feature_id: None,
                milestone_id: None,
                sdk_session_id: "sess-r-after-failure".to_string(),
                model: "sonnet".to_string(),
                prompt_hash: "deadbeef".to_string(),
                transcript_path: "runs/r-after-failure.jsonl".to_string(),
            },
        ),
    )
    .unwrap();
    assert_eq!(state.mission.status, MissionStatus::Failed);
}

// ---------------------------------------------------------------------------
// Decisions and user messages
// ---------------------------------------------------------------------------

#[test]
fn decisions_cap_at_ten_and_clear_pending_messages() {
    let mut state = fold(&[ev(1, created())]).unwrap();
    let mut seq = 1;
    let mut push = |state: &mut MissionState, kind: EventKind| {
        seq += 1;
        apply(state, &ev(seq, kind)).unwrap();
    };

    push(
        &mut state,
        EventKind::UserMessage {
            text: "msg-1".into(),
            interrupt: false,
        },
    );
    push(
        &mut state,
        EventKind::UserMessage {
            text: "msg-2".into(),
            interrupt: true,
        },
    );
    assert_eq!(state.pending_user_messages, vec!["msg-1", "msg-2"]);

    push(
        &mut state,
        EventKind::OrchestratorDecision {
            summary: "d-1".into(),
            detail: Some("why".into()),
        },
    );
    assert!(
        state.pending_user_messages.is_empty(),
        "decision consumes the queue"
    );
    assert_eq!(state.recent_decisions, vec!["d-1"]);

    for i in 2..=13 {
        push(
            &mut state,
            EventKind::OrchestratorDecision {
                summary: format!("d-{i}"),
                detail: None,
            },
        );
    }
    assert_eq!(state.recent_decisions.len(), 10, "capped at 10");
    let expected: Vec<String> = (4..=13).map(|i| format!("d-{i}")).collect();
    assert_eq!(
        state.recent_decisions, expected,
        "oldest dropped, newest last"
    );
}

// ---------------------------------------------------------------------------
// Config merge
// ---------------------------------------------------------------------------

#[test]
fn config_changed_deep_merges_patch() {
    let mut state = fold(&[ev(1, created())]).unwrap();
    apply(
        &mut state,
        &ev(
            2,
            EventKind::ConfigChanged {
                patch: json!({
                    "worker": { "model": "opus" },
                    "maxParallelWorkers": 3,
                    "denyPatterns": ["git push*"],
                    "claudeBinary": "/opt/claude"
                }),
            },
        ),
    )
    .unwrap();

    // Patched leaves changed...
    assert_eq!(state.config.worker.model, "opus");
    assert_eq!(state.config.max_parallel_workers, 3);
    assert_eq!(state.config.deny_patterns, vec!["git push*"]);
    assert_eq!(state.config.claude_binary.as_deref(), Some("/opt/claude"));
    // ...sibling leaves of merged objects survive...
    assert_eq!(state.config.worker.reasoning_effort, "medium");
    assert_eq!(state.config.worker.max_turns, Some(50));
    // ...and untouched sections are untouched.
    assert_eq!(
        state.config.orchestrator,
        MissionConfig::default().orchestrator
    );

    // Non-object patch values replace wholesale (null clears an Option).
    apply(
        &mut state,
        &ev(
            3,
            EventKind::ConfigChanged {
                patch: json!({ "claudeBinary": null }),
            },
        ),
    )
    .unwrap();
    assert_eq!(state.config.claude_binary, None);
}

#[test]
fn config_changed_invalid_patch_is_config_error() {
    let mut state = fold(&[ev(1, created())]).unwrap();
    let err = apply(
        &mut state,
        &ev(
            2,
            EventKind::ConfigChanged {
                patch: json!({ "maxParallelWorkers": "many" }),
            },
        ),
    )
    .unwrap_err();
    assert!(matches!(err, EngineError::Config(_)), "got {err:?}");
}

// ---------------------------------------------------------------------------
// Snapshots
// ---------------------------------------------------------------------------

#[test]
fn snapshot_round_trips_atomically() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");

    let state = fold_kinds(vec![
        created(),
        EventKind::PlanApproved {
            plan: plan(),
            base_sha: None,
        },
        EventKind::MilestoneStarted {
            milestone_id: "ms-1".into(),
            start_sha: "s".into(),
        },
        spawn("r-1", Some("f-1-1"), Some("ms-1")),
        completed("r-1", tokens(7, 3), Some(0.1)),
    ]);
    write_snapshot(&state, &path).unwrap();
    assert!(
        !path.with_file_name("state.json.tmp").exists(),
        "tmp file renamed away"
    );

    let loaded = read_snapshot(&path).unwrap();
    assert_eq!(
        serde_json::to_value(&state).unwrap(),
        serde_json::to_value(&loaded).unwrap()
    );

    // Overwrite works (rename over existing file).
    write_snapshot(&loaded, &path).unwrap();
    assert_eq!(read_snapshot(&path).unwrap().last_seq, state.last_seq);
}

// ---------------------------------------------------------------------------
// Wire format (§4.3 envelope): exact JSON shape, including key order
// ---------------------------------------------------------------------------

#[test]
fn event_wire_format_matches_plan() {
    let ts = base_ts(); // 2026-01-02T03:04:05Z

    let worker_completed = Event {
        seq: 412,
        ts,
        mission_id: "m-01".to_string(),
        kind: EventKind::WorkerCompleted {
            run_id: "r-1".to_string(),
            result: RunResult::Pass,
            tokens: TokenUsage {
                input: 10,
                output: 5,
                cache_read: 2,
                cache_write: 1,
            },
            cost_usd: Some(0.5),
            report: None,
        },
    };
    assert_eq!(
        serde_json::to_string(&worker_completed).unwrap(),
        r#"{"seq":412,"ts":"2026-01-02T03:04:05Z","missionId":"m-01","type":"worker.completed","payload":{"runId":"r-1","result":"pass","tokens":{"input":10,"output":5,"cacheRead":2,"cacheWrite":1},"costUsd":0.5}}"#
    );

    let milestone_started = Event {
        seq: 3,
        ts,
        mission_id: "m-01".to_string(),
        kind: EventKind::MilestoneStarted {
            milestone_id: "ms-1".to_string(),
            start_sha: "abc123".to_string(),
        },
    };
    assert_eq!(
        serde_json::to_string(&milestone_started).unwrap(),
        r#"{"seq":3,"ts":"2026-01-02T03:04:05Z","missionId":"m-01","type":"milestone.started","payload":{"milestoneId":"ms-1","startSha":"abc123"}}"#
    );

    let user_message = Event {
        seq: 9,
        ts,
        mission_id: "m-01".to_string(),
        kind: EventKind::UserMessage {
            text: "ship it".to_string(),
            interrupt: true,
        },
    };
    assert_eq!(
        serde_json::to_string(&user_message).unwrap(),
        r#"{"seq":9,"ts":"2026-01-02T03:04:05Z","missionId":"m-01","type":"user.message","payload":{"text":"ship it","interrupt":true}}"#
    );

    let paused = Event {
        seq: 10,
        ts,
        mission_id: "m-01".to_string(),
        kind: EventKind::MissionPaused {},
    };
    assert_eq!(
        serde_json::to_string(&paused).unwrap(),
        r#"{"seq":10,"ts":"2026-01-02T03:04:05Z","missionId":"m-01","type":"mission.paused","payload":{}}"#
    );

    // And back: the envelope deserializes to the same event.
    let parsed: Event =
        serde_json::from_str(&serde_json::to_string(&worker_completed).unwrap()).unwrap();
    assert_eq!(parsed.seq, 412);
    assert_eq!(parsed.kind.type_name(), "worker.completed");
}

// ---------------------------------------------------------------------------
// Property tests: fold == incremental apply; fold is deterministic
// ---------------------------------------------------------------------------

/// Script steps that always produce valid event sequences when interpreted
/// against the fixed 2x2 plan below.
#[derive(Debug, Clone)]
enum Action {
    MilestoneStarted(u8),
    FeatureStarted(u8),
    SpawnWorker(u8),
    SpawnValidator(u8),
    Message(u8),
    Complete(u8, u16, u16),
    FeatureCompleted(u8),
    FeatureFailed(u8),
    FeatureSkipped(u8),
    MilestoneValidating(u8),
    Finding(u8, u8),
    FixFeature(u8),
    Blocked(u8),
    Unblocked(u8),
    MilestoneCompleted(u8),
    Pause,
    Resume,
    UserMsg(String),
    Decision(String),
    ConfigPatch(u8),
    MissionValidating,
}

fn action_strategy() -> impl Strategy<Value = Action> {
    let text = || proptest::string::string_regex("[a-z0-9 ]{0,12}").unwrap();
    prop_oneof![
        any::<u8>().prop_map(Action::MilestoneStarted),
        any::<u8>().prop_map(Action::FeatureStarted),
        any::<u8>().prop_map(Action::SpawnWorker),
        any::<u8>().prop_map(Action::SpawnValidator),
        any::<u8>().prop_map(Action::Message),
        (any::<u8>(), any::<u16>(), any::<u16>()).prop_map(|(r, i, o)| Action::Complete(r, i, o)),
        any::<u8>().prop_map(Action::FeatureCompleted),
        any::<u8>().prop_map(Action::FeatureFailed),
        any::<u8>().prop_map(Action::FeatureSkipped),
        any::<u8>().prop_map(Action::MilestoneValidating),
        (any::<u8>(), any::<u8>()).prop_map(|(m, r)| Action::Finding(m, r)),
        any::<u8>().prop_map(Action::FixFeature),
        any::<u8>().prop_map(Action::Blocked),
        any::<u8>().prop_map(Action::Unblocked),
        any::<u8>().prop_map(Action::MilestoneCompleted),
        Just(Action::Pause),
        Just(Action::Resume),
        text().prop_map(Action::UserMsg),
        text().prop_map(Action::Decision),
        any::<u8>().prop_map(Action::ConfigPatch),
        Just(Action::MissionValidating),
    ]
}

/// Fixed plan for the property tests: ms-1 { f-1-1, f-1-2 }, ms-2 { f-2-1, f-2-2 }.
fn prop_plan() -> Plan {
    let feature = |t: &str| PlanFeature {
        title: t.into(),
        spec: t.into(),
        validation_criteria: vec![],
    };
    Plan {
        goal: "prop goal".into(),
        validation_contract: vec![],
        milestones: vec![
            PlanMilestone {
                title: "m1".into(),
                features: vec![feature("a"), feature("b")],
            },
            PlanMilestone {
                title: "m2".into(),
                features: vec![feature("c"), feature("d")],
            },
        ],
        command_grants: vec![],
        touch_set: vec![],
    }
}

/// Interpret actions into a contiguous, always-valid event list.
fn interpret(actions: &[Action]) -> Vec<Event> {
    const MS: [&str; 2] = ["ms-1", "ms-2"];
    const FS: [&str; 4] = ["f-1-1", "f-1-2", "f-2-1", "f-2-2"];
    let patches = [
        json!({ "worker": { "model": "opus" } }),
        json!({ "maxParallelWorkers": 4 }),
        json!({ "skipScrutiny": true, "validatorFunctional": { "reasoningEffort": "high" } }),
    ];

    let mut events = vec![
        ev(1, created()),
        ev(
            2,
            EventKind::PlanApproved {
                plan: prop_plan(),
                base_sha: None,
            },
        ),
    ];
    let mut runs: Vec<String> = Vec::new();
    let mut run_counter = 0u32;
    let mut fix_counter = 0u32;

    for action in actions {
        let seq = events.len() as u64 + 1;
        let ms = |i: u8| MS[i as usize % MS.len()].to_string();
        let fs = |i: u8| FS[i as usize % FS.len()].to_string();
        let kind = match action {
            Action::MilestoneStarted(m) => EventKind::MilestoneStarted {
                milestone_id: ms(*m),
                start_sha: "sha".into(),
            },
            Action::FeatureStarted(f) => EventKind::FeatureStarted { feature_id: fs(*f) },
            Action::SpawnWorker(f) => {
                run_counter += 1;
                let id = format!("r-{run_counter}");
                runs.push(id.clone());
                spawn(&id, Some(&fs(*f)), None)
            }
            Action::SpawnValidator(m) => {
                run_counter += 1;
                let id = format!("r-{run_counter}");
                runs.push(id.clone());
                spawn(&id, None, Some(&ms(*m)))
            }
            Action::Message(r) => {
                if runs.is_empty() {
                    continue;
                }
                let id = runs[*r as usize % runs.len()].clone();
                EventKind::WorkerMessage {
                    run_id: id,
                    tag: "text".into(),
                    content: "x".into(),
                }
            }
            Action::Complete(r, input, output) => {
                if runs.is_empty() {
                    continue;
                }
                let id = runs[*r as usize % runs.len()].clone();
                completed(
                    &id,
                    TokenUsage {
                        input: *input as u64,
                        output: *output as u64,
                        cache_read: 0,
                        cache_write: 0,
                    },
                    Some(0.01),
                )
            }
            Action::FeatureCompleted(f) => EventKind::FeatureCompleted {
                feature_id: fs(*f),
                commits: vec!["c".into()],
            },
            Action::FeatureFailed(f) => EventKind::FeatureFailed {
                feature_id: fs(*f),
                reason: "r".into(),
            },
            Action::FeatureSkipped(f) => EventKind::FeatureSkipped {
                feature_id: fs(*f),
                reason: "r".into(),
            },
            Action::MilestoneValidating(m) => EventKind::MilestoneValidating {
                milestone_id: ms(*m),
            },
            Action::Finding(m, r) => {
                if runs.is_empty() {
                    continue;
                }
                let id = runs[*r as usize % runs.len()].clone();
                EventKind::ValidationFinding {
                    milestone_id: ms(*m),
                    run_id: id,
                    finding: Finding {
                        subject: "s".into(),
                        severity: "minor".into(),
                        evidence: "e".into(),
                        suggested_fix: String::new(),
                        class: String::new(),
                    },
                }
            }
            Action::FixFeature(m) => {
                fix_counter += 1;
                EventKind::FixFeatureCreated {
                    milestone_id: ms(*m),
                    feature: fix_feature(&format!("fx-{fix_counter}")),
                }
            }
            Action::Blocked(m) => EventKind::MilestoneBlocked {
                milestone_id: ms(*m),
                reason: "r".into(),
            },
            Action::Unblocked(m) => EventKind::MilestoneUnblocked {
                milestone_id: ms(*m),
                reason: "r".into(),
            },
            Action::MilestoneCompleted(m) => EventKind::MilestoneCompleted {
                milestone_id: ms(*m),
                tag: None,
            },
            Action::Pause => EventKind::MissionPaused {},
            Action::Resume => EventKind::MissionResumed {},
            Action::UserMsg(text) => EventKind::UserMessage {
                text: text.clone(),
                interrupt: false,
            },
            Action::Decision(summary) => EventKind::OrchestratorDecision {
                summary: summary.clone(),
                detail: None,
            },
            Action::ConfigPatch(i) => EventKind::ConfigChanged {
                patch: patches[*i as usize % patches.len()].clone(),
            },
            Action::MissionValidating => EventKind::MissionValidating {},
        };
        events.push(ev(seq, kind));
    }
    events
}

proptest! {
    #[test]
    fn fold_equals_incremental_apply_and_is_deterministic(
        actions in proptest::collection::vec(action_strategy(), 0..60)
    ) {
        let events = interpret(&actions);

        let folded = fold(&events).unwrap();

        let mut incremental = fold(&events[..1]).unwrap();
        for event in &events[1..] {
            apply(&mut incremental, event).unwrap();
        }

        prop_assert_eq!(
            serde_json::to_value(&folded).unwrap(),
            serde_json::to_value(&incremental).unwrap()
        );

        // Determinism: folding the same events twice yields byte-identical JSON.
        let again = fold(&events).unwrap();
        prop_assert_eq!(
            serde_json::to_string(&folded).unwrap(),
            serde_json::to_string(&again).unwrap()
        );
    }
}

/// Final-gate findings are attributed to the reserved engine run id, which
/// must fold without a matching WorkerRun (any other unknown run id refuses).
#[test]
fn validation_finding_accepts_reserved_engine_run_id() {
    let finding = |run: &str| EventKind::ValidationFinding {
        milestone_id: "ms-1".to_string(),
        run_id: run.to_string(),
        finding: Finding {
            subject: "a-1".to_string(),
            severity: "major".to_string(),
            evidence: "command failed".to_string(),
            suggested_fix: String::new(),
            class: String::new(),
        },
    };

    let events = vec![
        ev(1, created()),
        ev(
            2,
            EventKind::PlanApproved {
                plan: plan(),
                base_sha: None,
            },
        ),
        ev(3, finding(kranz_engine::reducer::ENGINE_RUN_ID)),
    ];
    fold(&events).expect("engine run id must fold cleanly");

    let events = vec![
        ev(1, created()),
        ev(
            2,
            EventKind::PlanApproved {
                plan: plan(),
                base_sha: None,
            },
        ),
        ev(3, finding("r-77")),
    ];
    assert!(matches!(fold(&events), Err(EngineError::InvalidState(_))));
}

/// A colliding fixfeature id (e.g. a second re-plan minting the same
/// `<ms>-replan-N`) is rejected loudly rather than silently shadowing.
#[test]
fn fixfeature_created_rejects_a_duplicate_feature_id() {
    let mut state = fold(&[
        ev(1, created()),
        ev(
            2,
            EventKind::PlanApproved {
                plan: plan(),
                base_sha: None,
            },
        ),
    ])
    .expect("fold base");
    let ms = state.mission.milestones[0].id.clone();

    // First fixfeature with id "dup" — accepted.
    apply(
        &mut state,
        &ev(
            3,
            EventKind::FixFeatureCreated {
                milestone_id: ms.clone(),
                feature: fix_feature("dup"),
            },
        ),
    )
    .expect("first fixfeature accepted");

    // Second fixfeature reusing the same id — must error, not shadow.
    let err = apply(
        &mut state,
        &ev(
            4,
            EventKind::FixFeatureCreated {
                milestone_id: ms.clone(),
                feature: fix_feature("dup"),
            },
        ),
    )
    .expect_err("duplicate feature id must be rejected");
    assert!(
        matches!(err, EngineError::InvalidState(_)),
        "expected InvalidState, got {err:?}"
    );

    // Only one feature with that id landed.
    let count = state.mission.milestones[0]
        .features
        .iter()
        .filter(|f| f.id == "dup")
        .count();
    assert_eq!(count, 1, "no silent duplicate");
}
